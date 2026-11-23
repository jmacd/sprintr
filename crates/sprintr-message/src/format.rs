// SPDX-License-Identifier: Apache-2.0

//! One grammar shared by compile-time capture and runtime playback.

use std::fmt;

/// Maximum requested field width in the prototype.
pub const MAX_WIDTH: usize = 4096;

/// Invalid syntax, an unsupported specifier, or inconsistent arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatError(pub String);

impl fmt::Display for FormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for FormatError {}

/// The supported string and unsigned hexadecimal formatting operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spec {
    /// String insertion; source capture uses `:s`, stored formats omit it.
    String,
    /// Lowercase hexadecimal, optionally prefixed, padded, and width-qualified.
    Hex {
        /// Include the `0x` prefix.
        alternate: bool,
        /// Pad with zeros instead of spaces.
        zero_pad: bool,
        /// Minimum rendered width, including any prefix.
        width: usize,
    },
}

/// One placeholder; the name is a label, not a lookup into the argument list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    /// Source identifier or expression label; empty for positional source inputs.
    pub name: String,
    /// Original specifier, excluding the colon.
    pub format: String,
    /// Parsed formatting operation.
    pub spec: Spec,
}

/// Literal text or a positional placeholder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// Literal text with doubled braces unescaped.
    Literal(String),
    /// A placeholder consuming the next argument.
    Field(Field),
}

fn specifier(text: &str) -> Result<Spec, FormatError> {
    if text.is_empty() || text == "s" {
        return Ok(Spec::String);
    }
    let unsupported = || FormatError(format!("unsupported format specifier :{text}"));
    let Some(width) = text.strip_suffix('x') else {
        return Err(unsupported());
    };
    let alternate = width.starts_with('#');
    let width = width.strip_prefix('#').unwrap_or(width);
    let zero_pad = width.starts_with('0');
    if !width.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(unsupported());
    }
    let width = if width.is_empty() {
        0
    } else {
        width
            .parse::<usize>()
            .map_err(|_| FormatError("format width is too large".into()))?
    };
    if width > MAX_WIDTH {
        return Err(FormatError(format!(
            "format width exceeds prototype limit of {MAX_WIDTH}"
        )));
    }
    Ok(Spec::Hex {
        alternate,
        zero_pad,
        width,
    })
}

/// Parse placeholders and escaped braces without evaluating source expressions.
///
/// Empty/string specifiers and `[#][0][width]x` are supported. Capture macros
/// additionally require an explicit `:s` for string arguments.
pub fn parse(input: &str) -> Result<Vec<Part>, FormatError> {
    parse_impl(input, false)
}

/// Source-only `:?` captures Debug text. Like `:s`, it is omitted on the wire.
pub fn parse_capture(input: &str) -> Result<Vec<Part>, FormatError> {
    parse_impl(input, true)
}

fn parse_impl(input: &str, capture: bool) -> Result<Vec<Part>, FormatError> {
    let mut parts = Vec::new();
    let mut literal = String::new();
    let mut chars = input.char_indices().peekable();
    while let Some((offset, ch)) = chars.next() {
        match ch {
            '{' if chars.peek().is_some_and(|(_, next)| *next == '{') => {
                chars.next();
                literal.push('{');
            }
            '}' if chars.peek().is_some_and(|(_, next)| *next == '}') => {
                chars.next();
                literal.push('}');
            }
            '{' => {
                if !literal.is_empty() {
                    parts.push(Part::Literal(std::mem::take(&mut literal)));
                }
                let mut end = None;
                for (index, next) in chars.by_ref() {
                    if next == '{' {
                        return Err(FormatError(format!("nested opening brace at byte {index}")));
                    }
                    if next == '}' {
                        end = Some(index);
                        break;
                    }
                }
                let end = end
                    .ok_or_else(|| FormatError(format!("unclosed placeholder at byte {offset}")))?;
                let field = &input[offset + 1..end];
                let (name, format) = field.split_once(':').unwrap_or((field, ""));
                parts.push(Part::Field(Field {
                    name: name.into(),
                    format: format.into(),
                    spec: if capture && format == "?" {
                        Spec::String
                    } else {
                        specifier(format)?
                    },
                }));
            }
            '}' => {
                return Err(FormatError(format!(
                    "unmatched closing brace at byte {offset}"
                )));
            }
            other => literal.push(other),
        }
    }
    if !literal.is_empty() {
        parts.push(Part::Literal(literal));
    }
    Ok(parts)
}

/// Re-encode parsed parts as a stored format, escaping literal braces.
pub fn unparse(parts: &[Part]) -> String {
    let mut output = String::new();
    for part in parts {
        match part {
            Part::Literal(text) => {
                for ch in text.chars() {
                    match ch {
                        '{' => output.push_str("{{"),
                        '}' => output.push_str("}}"),
                        other => output.push(other),
                    }
                }
            }
            Part::Field(field) => {
                output.push('{');
                output.push_str(&field.name);
                if !field.format.is_empty() {
                    output.push(':');
                    output.push_str(&field.format);
                }
                output.push('}');
            }
        }
    }
    output
}
