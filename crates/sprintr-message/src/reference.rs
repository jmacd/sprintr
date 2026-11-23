// SPDX-License-Identifier: Apache-2.0

use crate::format::FormatError;
use std::fmt::Write as _;

/// The URI fragment addressing `key` as one JSON Pointer token under `defs`.
///
/// ```
/// assert_eq!(sprintr_message::definition_fragment("file:/tmp/~#%"),
///            "#/defs/file:~1tmp~1~0%23%25");
/// ```
pub fn definition_fragment(key: &str) -> String {
    let mut fragment = String::from("#/defs/");
    push_pointer_token(&mut fragment, key);
    fragment
}

/// Append one object key or array index to a JSON Pointer URI fragment, **without** a `/` separator.
/// Escape the token as a JSON Pointer *before* escaping bytes for the URI.
///
/// ```
/// let mut path = String::from("#/args/0/");
/// sprintr_message::push_pointer_token(&mut path, "src/~#");
/// assert_eq!(path, "#/args/0/src~1~0%23");
/// ```
pub fn push_pointer_token(fragment: &mut String, token: &str) {
    for byte in token.bytes() {
        match byte {
            b'~' => fragment.push_str("~0"),
            b'/' => fragment.push_str("~1"),
            b':' | b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' => {
                fragment.push(char::from(byte));
            }
            _ => write!(fragment, "%{byte:02X}").expect("string formatting"),
        }
    }
}

fn unsigned_id(text: &str) -> Option<u64> {
    if (text.len() > 1 && text.starts_with('0')) || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

pub(super) fn positive_id(text: &str) -> Option<u64> {
    unsigned_id(text).filter(|&id| id != 0)
}

pub(super) fn index(text: &str) -> Option<usize> {
    usize::try_from(unsigned_id(text)?).ok()
}

pub(super) enum Target {
    Definition {
        sequence: Option<u64>,
        key: String,
    },
    /// Decoded tokens *after* `/args/`, starting with the argument index.
    Argument(Vec<String>),
}

/// Decode the URI fragment before splitting the pointer or unescaping tokens.
pub(super) fn parse(target: &str) -> Result<Target, FormatError> {
    let invalid = || FormatError(format!("invalid reference {target:?}"));
    let (record, fragment) = target.split_once('#').ok_or_else(invalid)?;
    let sequence = if record.is_empty() {
        None
    } else {
        Some(unsigned_id(record).ok_or_else(invalid)?)
    };

    let mut pointer = Vec::with_capacity(fragment.len());
    let mut bytes = fragment.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let mut hex_digit = || {
                bytes
                    .next()
                    .and_then(|b| char::from(b).to_digit(16))
                    .ok_or_else(invalid)
            };
            pointer.push((hex_digit()? * 16 + hex_digit()?) as u8);
        } else if byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/?".contains(&byte) {
            pointer.push(byte);
        } else {
            return Err(invalid());
        }
    }
    let pointer = std::str::from_utf8(&pointer).map_err(|_| invalid())?;
    if let Some(token) = pointer.strip_prefix("/defs/").filter(|p| !p.contains('/')) {
        return Ok(Target::Definition {
            sequence,
            key: unescape(token).ok_or_else(invalid)?,
        });
    }
    if sequence.is_none()
        && let Some(path) = pointer.strip_prefix("/args/")
    {
        let tokens: Vec<_> = path
            .split('/')
            .map(unescape)
            .collect::<Option<_>>()
            .ok_or_else(invalid)?;
        index(&tokens[0]).ok_or_else(invalid)?;
        return Ok(Target::Argument(tokens));
    }
    Err(invalid())
}

fn unescape(token: &str) -> Option<String> {
    let mut key = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(ch) = chars.next() {
        key.push(match ch {
            '~' => match chars.next() {
                Some('0') => '~',
                Some('1') => '/',
                _ => return None,
            },
            ch => ch,
        });
    }
    Some(key)
}
