// SPDX-License-Identifier: Apache-2.0

//! Non-production, standalone JSON Lines recording and plain-text playback.
//!
//! No tracing subscriber or runtime is required.

#![doc = include_str!("../README.md")]

use std::collections::HashSet;
use std::io::{self, BufRead, Write};

mod attribute;
pub mod buffer;

use attribute::AttributeBinding;
use buffer::{HeapBuffer, RecordBuffer};
use slogr_message::BorrowedArgument;

pub use attribute::{Attribute, register_attribute};
pub use slogr_message::{
    Argument, Message, RelationDefinition, RelationKind, RelationType, Renderer,
    format::FormatError,
};

#[doc(hidden)]
pub use slogr_prototype_macros::{info as __info, message as __message};

/// Capture owned arguments without writing or preformatting them.
///
/// This convenience API allocates an owned snapshot. Use [`info!`] to encode
/// borrowed arguments directly without constructing this intermediate value.
///
/// ```rust
/// use slogr_prototype::message;
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
/// slogr_prototype::message!("hello {name}");
/// ```
///
/// Scenario: a numeric value is supplied for string encoding.
/// Guarantees: capture requires `AsRef<str>`, not eager Display formatting.
/// ```compile_fail
/// slogr_prototype::message!("{:s}", 42u32);
/// ```
///
/// Scenario: a signed integer is supplied for the unsigned hexadecimal subset.
/// Guarantees: unsupported integer types cannot silently lose signedness.
/// ```compile_fail
/// slogr_prototype::message!("{:x}", -1i32);
/// ```
///
/// Scenario: a placeholder has no corresponding positional expression.
/// Guarantees: argument count mismatches are compile errors.
/// ```compile_fail
/// slogr_prototype::message!("{:s}");
/// ```
///
/// Scenario: a positional expression has no corresponding placeholder.
/// Guarantees: surplus arguments cannot be silently discarded.
/// ```compile_fail
/// slogr_prototype::message!("hello", "Ada");
/// ```
///
/// Scenario: an attribute name has not been registered or declared.
/// Guarantees: unbound attribute handles are compile errors.
/// ```compile_fail
/// slogr_prototype::message!("{:s}", name = "Ada");
/// ```
///
/// Scenario: a string attribute is associated with a number.
/// Guarantees: its declared string type is enforced even with an untyped placeholder.
/// ```compile_fail
/// let account = slogr_prototype::register_attribute("account.name", "Account name").unwrap();
/// slogr_prototype::message!("{}", account = 42u32);
/// ```
///
/// Scenario: the source requests a format outside the prototype subset.
/// Guarantees: unsupported formats fail instead of degrading to text.
/// ```compile_fail
/// slogr_prototype::message!("{:?}", "Ada");
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
/// use slogr_prototype::{Logger, info};
/// let mut logger = Logger::new(Vec::new());
/// let name = "Ada";
/// let value = 42u32;
/// info!("hello {name:s}, value {value:04x}", logger: logger)?;
/// assert_eq!(
///     String::from_utf8(logger.into_inner()).unwrap(),
///     "{\"msg\":\"hello {name}, value {value:04x}\",\"args\":[\"Ada\",42]}\n"
/// );
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// The prototype does not yet record a severity field or filter messages.
/// Formats are checked and JSON-escaped at compile time. Arguments are borrowed
/// in a stack array and serialized directly into the logger's reusable buffer.
/// See [`message!`] for the supported format subset.
#[macro_export]
macro_rules! info {
    ($($input:tt)*) => {
        $crate::__info!($crate; $($input)*)
    };
}

/// A synchronous, caller-owned JSON Lines writer.
pub struct Logger<W, B = HeapBuffer> {
    writer: W,
    buffer: B,
    definitions: HashSet<usize>,
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
            definitions: HashSet::new(),
        }
    }

    /// Validate and encode an owned message before writing it.
    ///
    /// This path parses the format at runtime; [`info!`] bypasses that work
    /// using compile-time-validated metadata.
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
        prefix: &'static [u8],
        arguments: &[BorrowedArgument<'_>],
        bindings: &[AttributeBinding<'_>],
    ) -> io::Result<()> {
        encode_compiled(
            &mut self.buffer,
            prefix,
            arguments,
            bindings,
            &self.definitions,
        )?;
        self.writer.write_all(self.buffer.as_slice())?;
        mark_emitted(&mut self.definitions, bindings);
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

fn encode_compiled(
    buffer: &mut impl RecordBuffer,
    prefix: &'static [u8],
    arguments: &[BorrowedArgument<'_>],
    bindings: &[AttributeBinding<'_>],
    definitions: &HashSet<usize>,
) -> io::Result<()> {
    buffer.clear();
    buffer.try_atomic(|buffer| {
        buffer.write_all(prefix)?;
        serde_json::to_writer(&mut *buffer, arguments).map_err(json_error)?;
        let mut wrote_definition = false;
        for (index, binding) in bindings.iter().enumerate() {
            let relation = &binding.attribute.relation;
            if !definitions.contains(&relation.id)
                && !bindings[..index]
                    .iter()
                    .any(|previous| previous.attribute.relation.id == relation.id)
            {
                buffer.write_all(if wrote_definition {
                    b","
                } else {
                    b",\"defs\":{"
                })?;
                buffer.write_all(&relation.encoded_definition)?;
                wrote_definition = true;
            }
        }
        if wrote_definition {
            buffer.write_all(b"}")?;
        }
        if !bindings.is_empty() {
            buffer.write_all(b",\"refs\":[")?;
            for (index, binding) in bindings.iter().enumerate() {
                if index > 0 {
                    buffer.write_all(b",")?;
                }
                buffer.write_all(&binding.attribute.relation.reference_prefix)?;
                write!(buffer, "{}", binding.argument)?;
                buffer.write_all(b"\"}")?;
            }
            buffer.write_all(b"]")?;
        }
        buffer.write_all(b"}\n")
    })
}

fn mark_emitted(definitions: &mut HashSet<usize>, bindings: &[AttributeBinding<'_>]) {
    for binding in bindings {
        let id = binding.attribute.relation.id;
        if !definitions.contains(&id) {
            definitions.insert(id);
        }
    }
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
    use super::{HeapBuffer, RecordBuffer, encode_compiled, mark_emitted};
    pub use crate::attribute::{AttributeBinding, owned_message};
    pub use slogr_message::BorrowedArgument;
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::io::{self, Write};

    #[derive(Default)]
    struct StdoutState {
        buffer: HeapBuffer,
        definitions: HashSet<usize>,
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
        prefix: &'static [u8],
        arguments: &[BorrowedArgument<'_>],
        bindings: &[AttributeBinding<'_>],
    ) -> io::Result<()> {
        STDOUT_STATE
            .try_with(|state| {
                let mut state = state
                    .try_borrow_mut()
                    .map_err(|_| io::Error::other("stdout encoding buffer is already in use"))?;
                let StdoutState {
                    buffer,
                    definitions,
                } = &mut *state;
                encode_compiled(buffer, prefix, arguments, bindings, definitions)?;
                io::stdout().lock().write_all(buffer.as_slice())?;
                mark_emitted(definitions, bindings);
                Ok(())
            })
            .map_err(|_| io::Error::other("stdout encoding buffer is unavailable"))?
    }
}
