// SPDX-License-Identifier: Apache-2.0

//! Non-production, standalone JSON Lines recording and plain-text playback.
//!
//! No tracing subscriber or runtime is required.

#![doc = include_str!("../README.md")]

use std::collections::HashSet;
use std::io::{self, BufRead, Write};

mod attribute;
pub mod buffer;
mod statement;

use buffer::{HeapBuffer, RecordBuffer};
use sprintr_message::BorrowedArgument;
use statement::Statement;

pub use attribute::Attribute;
pub use sprintr_message::{
    Argument, ArgumentBinding, Definition, Level, Message, NamedEntity, RelationDefinition,
    RelationKind, RelationType, Renderer, StatementTemplate, format::FormatError,
};

#[doc(hidden)]
pub use sprintr_prototype_macros::{
    attribute as __attribute, error as __error, info as __info, message as __message,
    warn as __warn,
};

/// Declare a string attribute with compile-time-encoded metadata.
///
/// ```
/// use sprintr_prototype::{Attribute, attribute, message};
/// static ACCOUNT: Attribute = attribute!("account.name", "Account name");
/// assert_eq!(message!("{}", ACCOUNT = "Ada").render().unwrap(), "Ada\naccount.name = Ada");
/// ```
///
/// Scenario: a name cannot be represented in a canonical attribute placeholder.
/// Guarantees: invalid static names fail at compilation.
/// ```compile_fail
/// static BAD: sprintr_prototype::Attribute = sprintr_prototype::attribute!("bad:name", "Invalid");
/// ```
#[macro_export]
macro_rules! attribute {
    ($($input:tt)*) => { $crate::__attribute!($crate; $($input)*) };
}

/// Capture owned arguments without writing or preformatting them.
///
/// This convenience API allocates an owned snapshot. Use [`info!`] to encode
/// borrowed arguments directly without constructing this intermediate value.
/// Conflicting static declarations panic on this infallible convenience path;
/// [`info!`] returns the error instead.
///
/// ```rust
/// use sprintr_prototype::message;
/// let name = String::from("Ada");
/// let value = 42u32;
/// let record = message!("hello {name:s}, value {value:04x}");
/// assert_eq!(record.render().unwrap(), "hello Ada, value 002a");
/// assert_eq!(name, "Ada");
/// ```
///
/// Both implicit local captures and explicit positional expressions work.
/// Strings require `:s`; unsigned integers use `:x`, optionally with `#`,
/// zero padding, and a constant width up to 4096. `{{` and `}}` are literals.
/// Named/numbered argument overrides, debug formatting, and other specifiers
/// are not supported. Positional expression labels containing braces or
/// colons must instead be bound to local variables.
///
/// Scenario: a string capture omits its type designation.
/// Guarantees: unsupported implicit typing fails at compilation.
/// ```compile_fail
/// let name = "Ada";
/// sprintr_prototype::message!("hello {name}");
/// ```
///
/// Scenario: a numeric value is supplied for string encoding.
/// Guarantees: capture requires `AsRef<str>`, not eager Display formatting.
/// ```compile_fail
/// sprintr_prototype::message!("{:s}", 42u32);
/// ```
///
/// Scenario: a signed integer is supplied for the unsigned hexadecimal subset.
/// Guarantees: unsupported integer types cannot silently lose signedness.
/// ```compile_fail
/// sprintr_prototype::message!("{:x}", -1i32);
/// ```
///
/// Scenario: a placeholder has no corresponding positional expression.
/// Guarantees: argument count mismatches are compile errors.
/// ```compile_fail
/// sprintr_prototype::message!("{:s}");
/// ```
///
/// Scenario: a positional expression has no corresponding placeholder.
/// Guarantees: surplus arguments cannot be silently discarded.
/// ```compile_fail
/// sprintr_prototype::message!("hello", "Ada");
/// ```
///
/// Scenario: an attribute name has not been registered or declared.
/// Guarantees: unbound attribute handles are compile errors.
/// ```compile_fail
/// sprintr_prototype::message!("{:s}", name = "Ada");
/// ```
///
/// Scenario: a string attribute is associated with a number.
/// Guarantees: its declared string type is enforced even with an untyped placeholder.
/// ```compile_fail
/// static ACCOUNT: sprintr_prototype::Attribute = sprintr_prototype::attribute!("account.name", "Account name");
/// sprintr_prototype::message!("{}", ACCOUNT = 42u32);
/// ```
///
/// Scenario: a local attribute value would make a statement depend on runtime bindings.
/// Guarantees: statement metadata must be statically declared.
/// ```compile_fail
/// let account = sprintr_prototype::attribute!("account.name", "Account name");
/// sprintr_prototype::message!("{}", account = "Ada");
/// ```
///
/// Scenario: the source requests a format outside the prototype subset.
/// Guarantees: unsupported formats fail instead of degrading to text.
/// ```compile_fail
/// sprintr_prototype::message!("{:?}", "Ada");
/// ```
#[macro_export]
macro_rules! message {
    ($($input:tt)*) => {
        $crate::__message!($crate; $($input)*)
    };
}

/// Write a captured message as one JSON line, returning any output error.
///
/// Defaults to stdout. Pass `logger: expression` immediately after the format
/// to select a caller-owned [`Logger`]. No global logger is installed.
///
/// ```rust
/// use sprintr_prototype::{Logger, info};
/// let mut logger = Logger::new(Vec::new());
/// let name = "Ada";
/// let value = 42u32;
/// info!("hello {name:s}, value {value:04x}", logger: logger)?;
/// let record: sprintr_prototype::Message = serde_json::from_slice(&logger.into_inner())?;
/// assert_eq!(record.render().unwrap(), "hello Ada, value 002a\nlog.level = INFO");
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// The statement records a static `log.level:INFO` reference. No filtering is applied.
/// Formats are checked and JSON-escaped at compile time. Arguments are borrowed
/// in a stack array and serialized directly into the logger's reusable buffer.
/// See [`message!`] for the supported format subset.
#[macro_export]
macro_rules! info {
    ($($input:tt)*) => {
        $crate::__info!($crate; $($input)*)
    };
}

/// Write a message whose statement has the static named level `log.level:WARN`.
///
/// Accepts the same format, arguments, and optional `logger:` as [`info!`].
#[macro_export]
macro_rules! warn {
    ($($input:tt)*) => { $crate::__warn!($crate; $($input)*) };
}

/// Write a message whose statement has the static named level `log.level:ERROR`.
///
/// Accepts the same format, arguments, and optional `logger:` as [`info!`].
#[macro_export]
macro_rules! error {
    ($($input:tt)*) => { $crate::__error!($crate; $($input)*) };
}

/// A synchronous, caller-owned JSON Lines writer.
pub struct Logger<W, B = HeapBuffer> {
    writer: W,
    buffer: B,
    definitions: StreamState,
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
            definitions: StreamState::default(),
        }
    }

    /// Validate and encode an owned message before writing it.
    ///
    /// The snapshot must contain its required definitions. This path validates
    /// at runtime; [`info!`] bypasses that work using compile-time metadata.
    /// An I/O failure can still leave a partially written line in the sink.
    pub fn write(&mut self, message: &Message) -> io::Result<()> {
        message
            .validate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        self.buffer.clear();
        self.buffer.try_atomic(|buffer| {
            serde_json::to_writer(&mut *buffer, message).map_err(json_error)?;
            buffer.write_all(b"\n")
        })?;
        self.writer.write_all(self.buffer.as_slice())
    }

    /// Encode trusted compiler-generated metadata and typed arguments.
    #[doc(hidden)]
    pub fn __write_compiled(
        &mut self,
        statement: &'static Statement,
        arguments: &[BorrowedArgument<'_>],
    ) -> io::Result<()> {
        let id = statement.id()?;
        encode_compiled(
            &mut self.buffer,
            statement,
            id,
            arguments,
            &self.definitions,
        )?;
        self.writer.write_all(self.buffer.as_slice())?;
        self.definitions.mark_emitted(statement, id);
        Ok(())
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

#[derive(Default)]
struct StreamState {
    statements: HashSet<u64>,
    attributes: HashSet<&'static str>,
    levels: u8,
}

impl StreamState {
    fn mark_emitted(&mut self, statement: &Statement, id: u64) {
        if !self.statements.contains(&id) {
            self.statements.insert(id);
            if let Some(level) = statement.level {
                self.levels |= 1 << level as u8;
            }
            for attribute in statement.attributes {
                self.attributes.insert(attribute.name);
            }
        }
    }
}

fn encode_compiled(
    buffer: &mut impl RecordBuffer,
    statement: &Statement,
    id: u64,
    arguments: &[BorrowedArgument<'_>],
    definitions: &StreamState,
) -> io::Result<()> {
    buffer.clear();
    buffer.try_atomic(|buffer| {
        write!(buffer, "{{\"attrs\":{{\"log.statement\":{id}}},\"args\":")?;
        serde_json::to_writer(&mut *buffer, arguments).map_err(json_error)?;
        if !definitions.statements.contains(&id) {
            buffer.write_all(b",\"defs\":{")?;
            if definitions.statements.is_empty() {
                buffer.write_all(b"\"log.relation:log.statement\":{\"kind\":\"entity\",\"type\":\"object\",\"desc\":\"Logging statement\",\"anonymous\":true},")?;
            }
            if let Some(level) = statement.level
                && definitions.levels & (1 << level as u8) == 0
            {
                if definitions.levels == 0 {
                    buffer.write_all(b"\"log.relation:log.level\":{\"kind\":\"entity\",\"type\":\"object\",\"desc\":\"Logging level\"},")?;
                }
                buffer.write_all(statement.level_definition)?;
                buffer.write_all(b",")?;
            }
            for (index, attribute) in statement.attributes.iter().enumerate() {
                if !definitions.attributes.contains(attribute.name)
                    && !statement.attributes[..index].iter().any(|previous| previous.name == attribute.name)
                {
                    buffer.write_all(attribute.encoded_definition)?;
                    buffer.write_all(b",")?;
                }
            }
            write!(buffer, "\"log.statement:{id}\":")?;
            buffer.write_all(statement.encoded)?;
            buffer.write_all(b"}")?;
        }
        buffer.write_all(b"}\n")
    })
}

fn json_error(error: serde_json::Error) -> io::Error {
    io::Error::new(
        error.io_error_kind().unwrap_or(io::ErrorKind::InvalidData),
        error,
    )
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
    use super::{HeapBuffer, RecordBuffer, StreamState, encode_compiled};
    pub use crate::statement::{Statement, concatenate, encoded_len, owned_message};
    pub use sprintr_message::{BorrowedArgument, Level};
    use std::cell::RefCell;
    use std::io::{self, Write};

    #[derive(Default)]
    struct StdoutState {
        buffer: HeapBuffer,
        definitions: StreamState,
    }

    thread_local! {
        static STDOUT_STATE: RefCell<StdoutState> = RefCell::new(StdoutState::default());
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

    pub fn write_stdout(
        statement: &'static Statement,
        arguments: &[BorrowedArgument<'_>],
    ) -> io::Result<()> {
        let id = statement.id()?;
        STDOUT_STATE
            .try_with(|state| {
                let mut state = state
                    .try_borrow_mut()
                    .map_err(|_| io::Error::other("stdout encoding buffer is already in use"))?;
                let StdoutState {
                    buffer,
                    definitions,
                } = &mut *state;
                encode_compiled(buffer, statement, id, arguments, definitions)?;
                io::stdout().lock().write_all(buffer.as_slice())?;
                definitions.mark_emitted(statement, id);
                Ok(())
            })
            .map_err(|_| io::Error::other("stdout encoding buffer is unavailable"))?
    }
}
