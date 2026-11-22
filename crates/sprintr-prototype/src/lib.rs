// SPDX-License-Identifier: Apache-2.0

//! Non-production, standalone JSON Lines recording and plain-text playback.
//!
//! No tracing subscriber or runtime is required.

#![doc = include_str!("../README.md")]

use std::io::{self, BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

pub mod buffer;
mod declare;
mod entity;
mod runtime;
pub mod schema;
mod stack;
mod statement;
mod stream;
pub mod symbols;

use buffer::{HeapBuffer, RecordBuffer};
use entity::BorrowedArgument;
use runtime::{Collision, runtime};
use statement::Statement;
use stream::StreamState;

pub use declare::{Attribute, Entity, Named};
pub use entity::EntityHandle;
pub use sprintr_message::{
    Argument, ArgumentBinding, Definition, Message, RECORD, RELATION, RELATION_PREFIX,
    RelationDefinition, RelationKind, RelationType, Renderer, SOURCE, STACK, STATEMENT,
    STATEMENT_PREFIX, StatementSource, StatementTemplate, THREAD, format, format::FormatError,
};

pub use stack::{DEFAULT_STACK_DEPTH, MAX_STACK_DEPTH, set_stack_limit};

#[doc(hidden)]
pub use sprintr_prototype_macros::{
    attributes as __attributes, entities as __entities, record as __record,
};

/// Declare attributes. Each doc comment becomes the recorded description.
///
/// Values are `str` or `u64`. Handles may be lowercase so that logging calls
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
/// sprintr_prototype::info!("{} try {}", account_name = "Ada", retry_count = 2u8)?;
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

/// Record an INFO event as one JSON line, returning any output error.
///
/// Defaults to stdout. Pass `logger: expression` immediately after the format
/// to select a caller-owned [`Logger`].
///
/// ```rust
/// use sprintr_prototype::{Logger, info, replay};
/// let mut logger = Logger::new(Vec::new());
/// let name = String::from("Ada");
/// let value = 42u32;
/// info!("hello {name:s}, value {value:04x}", logger: logger)?;
/// let mut text = Vec::new();
/// replay(logger.into_inner().as_slice(), &mut text)?;
/// let text = String::from_utf8(text).unwrap();
/// assert!(text.ends_with("hello Ada, value 002a\nlog.level = INFO\n"));
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
/// overrides, debug formatting, and other specifiers are not supported.
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
/// sprintr_prototype::info!("{:?}", "Ada");
/// ```
#[macro_export]
macro_rules! info {
    ($($input:tt)*) => {
        $crate::__record!($crate; [$crate::schema::log_level::INFO]; $($input)*)
    };
}

/// Write a message whose statement has the static named level `log.level:WARN`.
///
/// Accepts the same format, arguments, and optional `logger:` as [`info!`].
#[macro_export]
macro_rules! warn {
    ($($input:tt)*) => {
        $crate::__record!($crate; [$crate::schema::log_level::WARN]; $($input)*)
    };
}

/// Write a message whose statement has the static named level `log.level:ERROR`.
///
/// Accepts the same format, arguments, and optional `logger:` as [`info!`].
#[macro_export]
macro_rules! error {
    ($($input:tt)*) => {
        $crate::__record!($crate; [$crate::schema::log_level::ERROR]; $($input)*)
    };
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

    /// Encode trusted compiler-generated metadata and typed arguments.
    #[doc(hidden)]
    pub fn __write_compiled(
        &mut self,
        statement: &'static Statement,
        stack: io::Result<&[u64]>,
        arguments: &[BorrowedArgument<'_>],
    ) -> io::Result<()> {
        let stack = stack?;
        self.start()?;
        self.state.write_compiled(
            &mut self.writer,
            &mut self.buffer,
            statement.resolve()?,
            stack,
            arguments,
        )
    }

    /// Announce the stream before its first record.
    fn start(&mut self) -> io::Result<()> {
        if self.state.started {
            return Ok(());
        }
        // Set first: the announcement is written through this logger.
        self.state.started = true;
        let result = announce(self);
        if result.is_err() {
            self.state.started = false;
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

fn announce<W: Write, B: RecordBuffer>(logger: &mut Logger<W, B>) -> io::Result<()> {
    info!(
        "process {} started logging with {} {}",
        logger: logger,
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
        warn!(
            "{kind:s} {declared:s} ({description:s}) collides with an earlier declaration; recorded as {recorded:s}",
            logger: logger,
        )?;
    }
    Ok(())
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

/// Read JSON Lines and write formatted messages without their structured envelope.
///
/// Invalid records report their one-based input line. Processing stops on the
/// first error; previously written messages are not rolled back.
pub fn replay(input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    let mut renderer = Renderer::default();
    for (index, line) in input.lines().enumerate() {
        let number = index + 1;
        let line =
            line.map_err(|error| io::Error::new(error.kind(), format!("line {number}: {error}")))?;
        let message: Message = serde_json::from_str(&line).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("line {number}: {error}"),
            )
        })?;
        let text = renderer.render(&message).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("line {number}: {error}"),
            )
        })?;
        writeln!(output, "{text}")?;
    }
    output.flush()
}

#[doc(hidden)]
pub mod __private {
    use super::{HeapBuffer, Logger, StreamState, announce};
    pub use crate::declare::{Declaration, RelationMeta};
    pub use crate::entity::BorrowedArgument;
    pub use crate::stack::capture_stack;
    pub use crate::statement::{Statement, concatenate, encoded_len, json_string, json_string_len};
    pub use linkme;
    use std::cell::RefCell;
    use std::io;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

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

    static STDOUT_STARTED: AtomicBool = AtomicBool::new(false);
    static STDOUT_START: Mutex<()> = Mutex::new(());

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
    fn start_stdout(state: &mut StdoutState) -> io::Result<()> {
        let _guard = STDOUT_START
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if STDOUT_STARTED.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut logger = Logger {
            writer: io::stdout(),
            buffer: std::mem::take(&mut state.buffer),
            state: std::mem::take(&mut state.definitions),
        };
        logger.state.started = true;
        let result = announce(&mut logger);
        state.buffer = logger.buffer;
        state.definitions = logger.state;
        result?;
        STDOUT_STARTED.store(true, Ordering::Release);
        Ok(())
    }

    pub fn write_stdout(
        statement: &'static Statement,
        stack: io::Result<&[u64]>,
        arguments: &[BorrowedArgument<'_>],
    ) -> io::Result<()> {
        let stack = stack?;
        STDOUT_STATE
            .try_with(|state| {
                let mut state = state
                    .try_borrow_mut()
                    .map_err(|_| io::Error::other("stdout encoding buffer is already in use"))?;
                if !STDOUT_STARTED.load(Ordering::Acquire) {
                    start_stdout(&mut state)?;
                }
                let resolved = statement.resolve()?;
                let StdoutState {
                    buffer,
                    definitions,
                } = &mut *state;
                definitions.write_compiled(&mut io::stdout(), buffer, resolved, stack, arguments)
            })
            .map_err(|_| io::Error::other("stdout encoding buffer is unavailable"))?
    }
}
