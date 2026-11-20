// SPDX-License-Identifier: Apache-2.0

//! The standalone prototype's message representation and format grammar.

pub mod format;

use format::{FormatError, Part, Spec};
use serde::{Deserialize, Serialize};

/// A captured value, retained as its native JSON type rather than formatted text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Argument {
    /// String data, including any literal braces or newlines.
    String(String),
    /// An unsigned integer, with the full `u64` range preserved.
    Unsigned(u64),
}

/// Borrowed arguments used by generated direct-encoding code.
#[doc(hidden)]
#[derive(Serialize)]
#[serde(untagged)]
pub enum BorrowedArgument<'a> {
    String(&'a str),
    Unsigned(u64),
}

/// One JSON Lines record, independent of callsites, entities, and tracing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[must_use = "a captured message must be recorded or rendered"]
pub struct Message {
    /// The stored format, with source labels and rendering specifiers.
    pub msg: String,
    /// Arguments in placeholder occurrence order.
    pub args: Vec<Argument>,
}

impl Message {
    /// Check format syntax, argument count, and argument types.
    pub fn validate(&self) -> Result<(), FormatError> {
        self.validate_parts(&format::parse(&self.msg)?)
    }

    fn validate_parts(&self, parts: &[Part]) -> Result<(), FormatError> {
        let fields = parts.iter().filter_map(|part| match part {
            Part::Field(field) => Some(field),
            Part::Literal(_) => None,
        });
        let count = fields.clone().count();
        if count != self.args.len() {
            return Err(FormatError(format!(
                "format has {count} placeholders but record has {} arguments",
                self.args.len()
            )));
        }
        for (index, (field, argument)) in fields.zip(&self.args).enumerate() {
            if !matches!(
                (field.spec, argument),
                (Spec::String, Argument::String(_)) | (Spec::Hex { .. }, Argument::Unsigned(_))
            ) {
                return Err(FormatError(format!(
                    "argument {index} ({}) must be {}",
                    field.name,
                    match field.spec {
                        Spec::String => "a string",
                        Spec::Hex { .. } => "an unsigned integer",
                    }
                )));
            }
        }
        Ok(())
    }

    /// Execute the stored format using only the captured argument values.
    pub fn render(&self) -> Result<String, FormatError> {
        let parts = format::parse(&self.msg)?;
        self.validate_parts(&parts)?;
        let mut arguments = self.args.iter();
        let mut output = String::new();
        for part in parts {
            match part {
                Part::Literal(text) => output.push_str(&text),
                Part::Field(field) => match (field.spec, arguments.next()) {
                    (Spec::String, Some(Argument::String(value))) => output.push_str(value),
                    (
                        Spec::Hex {
                            alternate,
                            zero_pad,
                            width,
                        },
                        Some(Argument::Unsigned(value)),
                    ) => {
                        let text = match (alternate, zero_pad) {
                            (true, true) => format!("{value:#0width$x}"),
                            (true, false) => format!("{value:#width$x}"),
                            (false, true) => format!("{value:0width$x}"),
                            (false, false) => format!("{value:width$x}"),
                        };
                        output.push_str(&text);
                    }
                    _ => unreachable!("argument count and types were validated"),
                },
            }
        }
        Ok(output)
    }
}
