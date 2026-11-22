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
    for byte in key.bytes() {
        match byte {
            b'~' => fragment.push_str("~0"),
            b'/' => fragment.push_str("~1"),
            b':' | b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' => {
                fragment.push(char::from(byte));
            }
            _ => write!(fragment, "%{byte:02X}").expect("string formatting"),
        }
    }
    fragment
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

/// Decode the URI fragment before interpreting JSON Pointer escapes. A ref
/// must select one definition, never a member inside it.
pub(super) fn parse(target: &str) -> Result<(Option<u64>, String), FormatError> {
    let invalid = || FormatError(format!("invalid definition reference {target:?}"));
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
    let token = pointer.strip_prefix("/defs/").ok_or_else(invalid)?;
    let mut key = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(ch) = chars.next() {
        key.push(match ch {
            '/' => return Err(invalid()),
            '~' => match chars.next() {
                Some('0') => '~',
                Some('1') => '/',
                _ => return Err(invalid()),
            },
            ch => ch,
        });
    }
    Ok((sequence, key))
}
