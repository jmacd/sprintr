// SPDX-License-Identifier: Apache-2.0

//! Non-production, standalone JSON Lines recording and plain-text playback.
//!
//! No tracing subscriber or runtime is required.

#![doc = include_str!("../README.md")]

use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

pub mod buffer;
mod context;
mod declare;
mod entity;
mod exception;
mod replay;
mod runtime;
pub mod schema;
mod span;
mod stack;
mod statement;
mod stream;
mod structured;
pub mod symbols;

use buffer::{HeapBuffer, RecordBuffer};
use entity::BorrowedArgument;
use runtime::{Collision, runtime};
use statement::Statement;
use stream::StreamState;

pub use context::{Context, ContextEntity, EntityContext};
pub use declare::{Attribute, Entity, Named};
pub use entity::BorrowedArgument as Value;
pub use entity::{EntityHandle, EntityIdentity, IdentityScopeRef, IdentityValues, Within};
pub use exception::{ExceptionRef, StructuredError};
pub use replay::{ReplayMode, replay};
pub use span::{Span, SpanOwner, SpanRef};
pub use sprintr_message::{
    Argument, ArgumentBinding, ContextRef, Definition, EncodeDefinition, IdScope, IdentityValue,
    Message, RECORD, RELATION, RELATION_PREFIX, RecordAddress, RelationDefinition, RelationKind,
    RelationType, Renderer, SOURCE, STACK, STATEMENT, STATEMENT_PREFIX, STRUCT_ENCODE,
    StatementSource, StatementTemplate, THREAD, format, format::FormatError,
};
pub use structured::{StructuredValue, ValueWriter};

pub use stack::{DEFAULT_STACK_DEPTH, MAX_STACK_DEPTH, set_stack_limit};

#[doc(hidden)]
pub use sprintr_prototype_macros::{
    attributes as __attributes, entities as __entities, exception as __exception,
    record as __record, span as __span, structured as __structured,
};

/// Describe how a [`StructuredValue`] formats itself. Call `.encode(out)` on
/// the result inside the trait implementation. Log the value directly with `{}`.
#[macro_export]
macro_rules! structured {
    ($($input:tt)*) => { $crate::__structured!($crate; []; $($input)*) };
}

/// Declare attributes. Each doc comment becomes the recorded description.
///
/// Values are `str`, `u64`, or `entity(relation)` for an entity-valued argument.
/// Handles may be lowercase so that logging calls
/// read like assignments: `info!("{}", account_name = name)`.
///
/// ```
/// sprintr_prototype::attributes! {
///     /// Account name
///     pub account_name: str = "account.name";
///     /// Retry attempt
///     pub retry_count: u64 = "retry.count";
/// }
///
/// sprintr_prototype::start()?;
/// sprintr_prototype::info!("{} try {}", account_name = "Ada", retry_count = 2u8);
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// Declarations are collected at link time and resolved when logging starts.
/// Names are process-wide; dots have no scoping meaning. If two declarations
/// of one name differ, the first keeps it and later ones are recorded under a
/// content-derived suffix such as `account.name_1a2b3c4d`, reported in-stream
/// when each output starts. Equal declarations in different crates share a name.
///
/// Scenario: a name cannot be represented in a canonical attribute placeholder.
/// Guarantees: invalid static names fail at compilation.
/// ```compile_fail
/// sprintr_prototype::attributes! {
///     /// Invalid
///     bad: str = "bad:name";
/// }
/// ```
///
/// Scenario: the root relation is redeclared.
/// Guarantees: `log.relation` defines relations and cannot itself be redefined.
/// ```compile_fail
/// sprintr_prototype::attributes! {
///     /// Invalid
///     root: str = "log.relation";
/// }
/// ```
///
/// Scenario: a declaration has no description.
/// Guarantees: every recorded relation describes itself.
/// ```compile_fail
/// sprintr_prototype::attributes! {
///     account_name: str = "account.name";
/// }
/// ```
#[macro_export]
macro_rules! attributes {
    ($($input:tt)*) => { $crate::__attributes!($crate; $($input)*); };
}

/// Declare entity relations and their statically named values.
///
/// ```
/// sprintr_prototype::entities! {
///     /// Deployment environment
///     #[named(global)]
///     pub environment = "deployment.environment" {
///         /// Production traffic
///         PRODUCTION,
///         /// Pre-production testing
///         STAGING,
///     }
/// }
///
/// assert_eq!(environment.name(), "deployment.environment");
/// assert_eq!(environment::STAGING.description(), "Pre-production testing");
/// ```
///
/// A relation with named values also declares a module of the same name
/// holding them. `#[anonymous]` relations, such as [`schema::log_statement`],
/// have no named values. Create a user-defined anonymous object with
/// `relation.new(value)?` and pass the resulting handle to a `{}` placeholder.
#[macro_export]
macro_rules! entities {
    ($($input:tt)*) => { $crate::__entities!($crate; $($input)*); };
}

/// Record an INFO event. Instrumentation returns `()`; the SDK diagnoses failures.
///
/// After [`start`], the default output is stdout. [`Logger::with_default`]
/// installs a different output for a synchronous scope on this thread.
///
/// ```rust
/// use sprintr_prototype::{info, start};
/// start()?;
/// let name = String::from("Ada");
/// let value = 42u32;
/// info!("hello {name:s}, value {value:04x}");
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// The macro borrows arguments in a stack array and encodes them directly into
/// the logger's reusable buffer. The statement has a static `log.level:INFO`.
/// Each output begins with an `INFO` record announcing the process and version,
/// followed by a `WARN` for each declaration renamed because of a collision.
/// Stdout announces once per process.
///
/// Both implicit local captures and explicit positional expressions work.
/// Strings require `:s`; unsigned integers use `:x`, optionally with `#`,
/// zero padding, and a constant width up to 4096. `{{` and `}}` are literals.
/// A positional `attribute = value` argument associates the placeholder with a
/// declared attribute, whose type also permits `{}`. Named/numbered argument
/// overrides and other specifiers are not supported. `:?` captures Debug text.
/// Positional expression labels containing braces or colons must instead be
/// bound to local variables.
///
/// Scenario: a string capture omits its type designation.
/// Guarantees: unsupported implicit typing fails at compilation.
/// ```compile_fail
/// let name = "Ada";
/// sprintr_prototype::info!("hello {name}");
/// ```
///
/// Scenario: a numeric value is supplied for string encoding.
/// Guarantees: capture requires `AsRef<str>`, not eager Display formatting.
/// ```compile_fail
/// sprintr_prototype::info!("{:s}", 42u32);
/// ```
///
/// Scenario: a signed integer is supplied for the unsigned hexadecimal subset.
/// Guarantees: unsupported integer types cannot silently lose signedness.
/// ```compile_fail
/// sprintr_prototype::info!("{:x}", -1i32);
/// ```
///
/// Scenario: a placeholder has no corresponding positional expression.
/// Guarantees: argument count mismatches are compile errors.
/// ```compile_fail
/// sprintr_prototype::info!("{:s}");
/// ```
///
/// Scenario: a positional expression has no corresponding placeholder.
/// Guarantees: surplus arguments cannot be silently discarded.
/// ```compile_fail
/// sprintr_prototype::info!("hello", "Ada");
/// ```
///
/// Scenario: an attribute has not been declared.
/// Guarantees: unbound attribute handles are compile errors.
/// ```compile_fail
/// sprintr_prototype::info!("{:s}", name = "Ada");
/// ```
///
/// Scenario: a local variable is used as an attribute handle.
/// Guarantees: statement metadata must be statically declared.
/// ```compile_fail
/// let account_name = "account.name";
/// sprintr_prototype::info!("{}", account_name = "Ada");
/// ```
///
/// Scenario: a string attribute is associated with a number.
/// Guarantees: its declared type is enforced even with an untyped placeholder.
/// ```compile_fail
/// sprintr_prototype::attributes! {
///     /// Account name
///     account_name: str = "account.name";
/// }
/// sprintr_prototype::info!("{}", account_name = 42u32);
/// ```
///
/// Scenario: an unsigned attribute uses the string specifier.
/// Guarantees: placeholder specifiers must agree with declared types.
/// ```compile_fail
/// sprintr_prototype::attributes! {
///     /// Retry attempt
///     retry_count: u64 = "retry.count";
/// }
/// sprintr_prototype::info!("{:s}", retry_count = 2u8);
/// ```
///
/// Scenario: a string attribute uses the hexadecimal specifier.
/// Guarantees: placeholder specifiers must agree with declared types.
/// ```compile_fail
/// sprintr_prototype::attributes! {
///     /// Account name
///     account_name: str = "account.name";
/// }
/// sprintr_prototype::info!("{:x}", account_name = "Ada");
/// ```
///
/// Scenario: the source requests a format outside the prototype subset.
/// Guarantees: unsupported formats fail instead of degrading to text.
/// ```compile_fail
/// sprintr_prototype::info!("{:b}", 42u32);
/// ```
#[macro_export]
macro_rules! info {
    ($($input:tt)*) => {
        $crate::__private::report($crate::__record!($crate; [$crate::schema::log_level::INFO]; $($input)*))
    };
}

/// Write a message whose statement has the static named level `log.level:WARN`.
///
/// Accepts the same format and arguments as [`info!`].
#[macro_export]
macro_rules! warn {
    ($($input:tt)*) => {
        $crate::__private::report($crate::__record!($crate; [$crate::schema::log_level::WARN]; $($input)*))
    };
}

/// Write a message whose statement has the static named level `log.level:ERROR`.
///
/// Accepts the same format and arguments as [`info!`].
#[macro_export]
macro_rules! error {
    ($($input:tt)*) => {
        $crate::__private::report($crate::__record!($crate; [$crate::schema::log_level::ERROR]; $($input)*))
    };
}

/// Record the creation of a detached context using the current bindings.
/// The returned [`Context`] has its own [`Context::origin`] if the output
/// accepted its creation record. Creating it does not enter it.
#[macro_export]
macro_rules! context {
    ($($input:tt)*) => {
        $crate::__private::created_context(
            $crate::Context::current(),
            $crate::__record!($crate; [$crate::schema::log_kind::CONTEXT_START]; $($input)*)
        )
    };
}

/// Record a span start and return its sole close owner. Entering the span is
/// explicit, using [`Span::in_scope`]; closing records a separate end statement.
#[macro_export]
macro_rules! span {
    ($($input:tt)*) => { $crate::__span!($crate; [$crate::schema::log_kind::SPAN_START]; $($input)*) };
}

/// Capture one error occurrence and return the supplied tuple variant. The
/// optional final `cause = &inner` is a link, not a message placeholder.
#[macro_export]
macro_rules! exception {
    ($variant:path, $($input:tt)*) => { $crate::__exception!($variant; $crate; [$crate::schema::log_kind::EXCEPTION]; $($input)*) };
}

#[derive(Clone, Copy)]
struct EventArgs<'a> {
    values: &'a [BorrowedArgument<'a>],
    kind: EventKind<'a>,
}

#[derive(Clone, Copy)]
enum EventKind<'a> {
    Ordinary,
    SpanStart(&'a span::Start),
    Exception(exception::Occurrence),
}
/// A synchronous, caller-owned JSON Lines writer.
pub struct Logger<W, B = HeapBuffer> {
    writer: W,
    buffer: B,
    state: StreamState,
}

impl<W: Write> Logger<W> {
    /// Wrap an output writer with a reusable, growable heap buffer.
    pub fn new(writer: W) -> Self {
        Self::with_buffer(writer, HeapBuffer::default())
    }
}

impl<W: Write, B: RecordBuffer> Logger<W, B> {
    /// Select storage, including a fixed [`buffer::StackBuffer`].
    pub fn with_buffer(writer: W, buffer: B) -> Self {
        Self {
            writer,
            buffer,
            state: StreamState::default(),
        }
    }

    /// Run `f` with this logger as the current thread's default. Nested scopes
    /// restore their predecessor, including during unwinding. The scope does
    /// not start the logger, propagate to spawned threads, or follow async tasks.
    pub fn with_default<R>(&mut self, f: impl FnOnce() -> R) -> R {
        let logger = std::cell::RefCell::new(self);
        CURRENT.set(
            &|statement, stack, args| {
                logger
                    .try_borrow_mut()
                    .map_err(|_| io::Error::other("scoped logger is already in use"))?
                    .write_compiled(statement, stack, args)
            },
            f,
        )
    }

    fn write_compiled(
        &mut self,
        statement: &'static Statement,
        stack: io::Result<&[u64]>,
        arguments: EventArgs<'_>,
    ) -> io::Result<Option<RecordAddress>> {
        if self.state.startup.is_none() {
            return unstarted(statement, arguments);
        }
        self.state
            .write_compiled(
                &mut self.writer,
                &mut self.buffer,
                statement.resolve()?,
                stack?,
                arguments,
            )
            .map(Some)
    }

    /// Attempt startup once. Repeated calls return the original outcome.
    /// After a partial announcement, subsequent events continue best-effort.
    pub fn start(&mut self) -> io::Result<()> {
        if let Some(result) = &self.state.startup {
            return startup_result(result);
        }
        // The announcement uses this output's normal encoder and definition table.
        self.state.startup = Some(Ok(()));
        let result = self.with_default(announce);
        if let Err(error) = &result {
            self.state.startup = Some(Err(io::Error::new(error.kind(), error.to_string())));
        }
        result
    }

    /// Flush buffered output, reporting any failure.
    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }

    /// Recover the writer. Call [`Self::flush`] first if it buffers output.
    pub fn into_inner(self) -> W {
        self.writer
    }
}

/// Start the process-wide stdout output. Logging before this call prints the
/// event and an unstarted-SDK warning to stderr. Logging never initiates startup.
pub fn start() -> io::Result<()> {
    __private::start_stdout()
}

fn startup_result(result: &io::Result<()>) -> io::Result<()> {
    result
        .as_ref()
        .map(|_| ())
        .map_err(|e| io::Error::new(e.kind(), e.to_string()))
}

fn unstarted(
    statement: &'static Statement,
    args: EventArgs<'_>,
) -> io::Result<Option<RecordAddress>> {
    use std::cell::RefCell;
    thread_local! { static FALLBACK: RefCell<()> = const { RefCell::new(()) }; }
    FALLBACK
        .try_with(|guard| {
            let _guard = guard
                .try_borrow_mut()
                .map_err(|_| io::Error::other("recursive pre-start logging"))?;
            // Use the ordinary encoder and reader so that Debug and structured
            // formatters run exactly once and use the same presentation as replay.
            let mut bytes = Vec::new();
            entity::with_preview_record(|record| {
                StreamState::default().write_record(
                    &mut bytes,
                    &mut HeapBuffer::default(),
                    statement.resolve()?,
                    &[],
                    args,
                    record,
                )
            })?;
            let event: Message = serde_json::from_slice(&bytes).map_err(json_error)?;
            let text = event.render().map_err(io::Error::other)?;
            writeln!(
                io::stderr().lock(),
                "sprintr: SDK has not been started; {text}"
            )?;
            Ok(None)
        })
        .map_err(|_| io::Error::other("pre-start logging unavailable"))?
}

type Dispatch<'scope> = dyn Fn(&'static Statement, io::Result<&[u64]>, EventArgs<'_>) -> io::Result<Option<RecordAddress>>
    + 'scope;
scoped_tls_hkt::scoped_thread_local!(static CURRENT: for<'a> &'a Dispatch<'a>);

fn announce() -> io::Result<()> {
    runtime()
        .check_schema()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    crate::__record!(crate; [crate::schema::log_level::INFO];
        "process {} started logging with {} {}",
        schema::process_pid = std::process::id(),
        schema::telemetry_sdk_name = "sprintr",
        schema::telemetry_sdk_version = env!("CARGO_PKG_VERSION"),
    )?;
    for collision in &runtime().collisions {
        let Collision {
            kind,
            declared,
            description,
            recorded,
        } = collision;
        crate::__record!(crate; [crate::schema::log_level::WARN];
            "{kind:s} {declared:s} ({description:s}) collides with an earlier declaration; recorded as {recorded:s}",
        )?;
    }
    Ok(())
}

/// Last path component outside generic parameters or a qualified trait receiver.
fn rust_leaf_name(name: &str) -> Option<&str> {
    let mut depth = 0usize;
    let mut start = 0;
    for (i, b) in name.bytes().enumerate() {
        match b {
            b'<' => depth += 1,
            b'>' if !name[..i].ends_with('-') => depth = depth.saturating_sub(1),
            b':' if depth == 0 && name[i..].starts_with("::") && !name[i..].starts_with("::<") => {
                start = i + 2
            }
            _ => {}
        }
    }
    let leaf = name[start..].split('<').next()?.trim_end_matches("::");
    (!leaf.is_empty()).then_some(leaf)
}

fn json_error(error: serde_json::Error) -> io::Error {
    io::Error::new(
        error.io_error_kind().unwrap_or(io::ErrorKind::InvalidData),
        error,
    )
}

fn timestamp_now() -> io::Result<u64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?;
    u64::try_from(elapsed.as_nanos())
        .map_err(|_| io::Error::other("Unix timestamp exceeds u64 nanoseconds"))
}

#[doc(hidden)]
pub mod __private {
    use super::{
        Context, HeapBuffer, Logger, RecordAddress, StreamState, startup_result, unstarted,
    };
    pub use crate::declare::{Declaration, RelationMeta};
    pub use crate::entity::BorrowedArgument;
    pub use crate::exception::{ErrorSite, OwnedField, RetainedArgument, capture_exception};
    pub use crate::stack::capture_stack;
    pub use crate::statement::{
        NestedTemplate, Statement, concatenate, encoded_len, json_string, json_string_len,
    };
    pub use crate::structured::{ScalarArgument, StructuredArgument, formatted_value};
    pub use linkme;
    use std::cell::RefCell;
    use std::io::{self, Write};
    use std::sync::OnceLock;

    /// Every declaration in the program, collected at link time.
    #[linkme::distributed_slice]
    pub static DECLARATIONS: [Declaration];

    #[derive(Default)]
    struct StdoutState {
        buffer: HeapBuffer,
        definitions: StreamState,
    }

    thread_local! {
        static STDOUT_STATE: RefCell<StdoutState> = RefCell::new(StdoutState::default());
    }

    static STDOUT_START: OnceLock<io::Result<()>> = OnceLock::new();

    #[inline]
    pub(super) fn accepted_address(
        result: io::Result<Option<RecordAddress>>,
    ) -> Option<RecordAddress> {
        result.unwrap_or_else(|error| {
            // stderr itself may be unavailable; instrumentation must still return.
            let _ = writeln!(
                io::stderr().lock(),
                "sprintr: could not record event: {error}"
            );
            None
        })
    }

    #[inline]
    pub fn report(result: io::Result<Option<RecordAddress>>) {
        let _ = accepted_address(result);
    }

    pub fn created_context(
        snapshot: Context,
        result: io::Result<Option<RecordAddress>>,
    ) -> Context {
        snapshot.with_origin(accepted_address(result))
    }

    mod sealed {
        pub trait Sealed {}
    }

    pub trait Unsigned: sealed::Sealed {
        fn as_u64(&self) -> u64;
    }

    macro_rules! unsigned_types {
        ($($ty:ty),*) => {
            $(
                impl sealed::Sealed for $ty {}
                impl Unsigned for $ty {
                    fn as_u64(&self) -> u64 {
                        u64::from(*self)
                    }
                }
            )*
        };
    }
    unsigned_types!(u8, u16, u32, u64);

    impl<T: Unsigned + ?Sized> sealed::Sealed for &T {}
    impl<T: Unsigned + ?Sized> Unsigned for &T {
        fn as_u64(&self) -> u64 {
            T::as_u64(self)
        }
    }

    pub fn unsigned(value: &(impl Unsigned + ?Sized)) -> u64 {
        value.as_u64()
    }

    /// Announce stdout once per process, before any thread's first record.
    pub(super) fn start_stdout() -> io::Result<()> {
        startup_result(STDOUT_START.get_or_init(|| {
            STDOUT_STATE
                .try_with(|state| {
                    let mut state = state.try_borrow_mut().map_err(|_| {
                        io::Error::other("stdout encoding buffer is already in use")
                    })?;
                    let mut logger = Logger::new(io::stdout());
                    let result = logger.start();
                    state.buffer = logger.buffer;
                    state.definitions = logger.state;
                    result
                })
                .map_err(|_| io::Error::other("stdout encoding buffer is unavailable"))?
        }))
    }

    pub fn write_event(
        statement: &'static Statement,
        stack: io::Result<&[u64]>,
        arguments: &[BorrowedArgument<'_>],
    ) -> io::Result<Option<RecordAddress>> {
        dispatch(
            statement,
            stack,
            super::EventArgs {
                values: arguments,
                kind: super::EventKind::Ordinary,
            },
        )
    }

    pub fn start_span(
        statement: &'static Statement,
        stack: io::Result<&[u64]>,
        arguments: &[BorrowedArgument<'_>],
    ) -> crate::Span {
        let base = Context::current();
        let start = super::span::Start::new(base.span());
        let reference = match start {
            Ok(start) => accepted_address(dispatch(
                statement,
                stack,
                super::EventArgs {
                    values: arguments,
                    kind: super::EventKind::SpanStart(&start),
                },
            ))
            .map(|address| start.accepted(address)),
            Err(error) => {
                report(Err(error));
                None
            }
        };
        crate::Span::new(base, reference)
    }

    pub(super) fn dispatch(
        statement: &'static Statement,
        stack: io::Result<&[u64]>,
        arguments: super::EventArgs<'_>,
    ) -> io::Result<Option<RecordAddress>> {
        if super::CURRENT.is_set() {
            return super::CURRENT.with(|dispatch| dispatch(statement, stack, arguments));
        }
        if STDOUT_START.get().is_none() {
            return unstarted(statement, arguments);
        }
        let stack = stack?;
        STDOUT_STATE
            .try_with(|state| {
                let mut state = state
                    .try_borrow_mut()
                    .map_err(|_| io::Error::other("stdout encoding buffer is already in use"))?;
                let resolved = statement.resolve()?;
                let StdoutState {
                    buffer,
                    definitions,
                } = &mut *state;
                definitions
                    .write_compiled(&mut io::stdout(), buffer, resolved, stack, arguments)
                    .map(Some)
            })
            .map_err(|_| io::Error::other("stdout encoding buffer is unavailable"))?
    }
}
