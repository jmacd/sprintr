// SPDX-License-Identifier: Apache-2.0

use crate::declare::{Named, RelationMeta};
use crate::entity::BorrowedArgument;
use crate::format::{self, Part};
use crate::runtime::runtime;
use crate::{Definition, Message, STATEMENT, STATEMENT_PREFIX, StatementTemplate};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Default number of caller PCs, excluding the static statement location.
pub const DEFAULT_STACK_DEPTH: usize = 10;
/// Capacity of the stack-local capture buffer used by the macros.
pub const MAX_STACK_DEPTH: usize = 64;
static STACK_LIMIT: AtomicUsize = AtomicUsize::new(DEFAULT_STACK_DEPTH);

/// Set the process-wide limit on caller PCs per event. Zero skips unwinding.
pub fn set_stack_limit(limit: usize) -> io::Result<()> {
    if limit > MAX_STACK_DEPTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("stack limit {limit} exceeds {MAX_STACK_DEPTH}"),
        ));
    }
    STACK_LIMIT.store(limit, Ordering::Relaxed);
    Ok(())
}

/// Called directly at the macro invocation, before entering the logger.
#[doc(hidden)]
#[inline(never)]
pub fn capture_stack(pcs: &mut [u64; MAX_STACK_DEPTH]) -> io::Result<&[u64]> {
    let limit = STACK_LIMIT.load(Ordering::Relaxed);
    if limit == 0 {
        return Ok(&[]);
    }
    let marker = 0u8;
    let boundary = std::ptr::from_ref(std::hint::black_box(&marker)).addr();
    let mut len = 0;
    let mut past_capture = false;
    let mut saw_callsite = false;
    backtrace::trace(|frame| {
        // Native stacks grow down. If the backend lacks SPs, use the function
        // boundary instead. Neither check requires debug-symbol lookup.
        let sp = frame.sp().addr();
        if (sp > boundary || past_capture) && !frame.ip().is_null() {
            if saw_callsite {
                pcs[len] = frame.ip().addr() as u64;
                len += 1;
            } else {
                saw_callsite = true; // frame zero is already in the statement
            }
        } else if sp == 0 && frame.symbol_address().addr() == capture_stack as *const () as usize {
            past_capture = true;
        }
        len < limit
    });
    if !saw_callsite {
        return Err(io::Error::other(
            "unwinder could not locate the logging callsite",
        ));
    }
    Ok(&pcs[..len])
}

/// Compiler-generated anonymous entity, unique to a macro callsite.
#[doc(hidden)]
pub struct Statement {
    encoded: &'static [u8],
    attributes: &'static [&'static RelationMeta],
    values: &'static [&'static Named],
    resolved: OnceLock<Result<Resolved, String>>,
}

/// A statement's identity, its template under recorded names, and the
/// definitions a stream needs before the template.
pub(crate) struct Resolved {
    pub(crate) id: u64,
    pub(crate) template: Cow<'static, [u8]>,
    pub(crate) definitions: Box<[usize]>,
}

impl Statement {
    pub const fn new(
        encoded: &'static [u8],
        attributes: &'static [&'static RelationMeta],
        values: &'static [&'static Named],
    ) -> Self {
        Self {
            encoded,
            attributes,
            values,
            resolved: OnceLock::new(),
        }
    }

    pub(crate) fn resolve(&self) -> io::Result<&Resolved> {
        self.resolved
            .get_or_init(|| self.resolve_once())
            .as_ref()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.clone()))
    }

    fn resolve_once(&self) -> Result<Resolved, String> {
        let runtime = runtime();
        let mut definitions = Vec::new();
        let mut add = |index: usize| {
            if !definitions.contains(&index) {
                definitions.push(index);
            }
        };
        add(runtime.relation(crate::schema::log_statement.__meta())?);
        add(runtime.relation(crate::schema::log_stack.__meta())?);
        add(runtime.relation(crate::schema::log_source.__meta())?);
        let mut renamed = false;
        let mut attrs = BTreeMap::new();
        for value in self.values {
            let relation = runtime.relation(value.relation().__meta())?;
            let named = runtime.named(value)?;
            add(relation);
            add(named);
            let relation = &runtime.definitions[relation].name;
            let named = &runtime.definitions[named].name;
            renamed |= relation != value.relation().name() || named != value.name();
            if attrs.insert(relation.clone(), named.clone()).is_some() {
                return Err(format!("statement has more than one {relation} value"));
            }
        }
        let mut names = Vec::new();
        for attribute in self.attributes {
            let index = runtime.relation(attribute)?;
            add(index);
            let name = runtime.definitions[index].name.as_str();
            renamed |= name != attribute.name;
            names.push(name);
        }
        let template = if renamed {
            Cow::Owned(rename(self.encoded, attrs, &names)?)
        } else {
            Cow::Borrowed(self.encoded)
        };
        let id = NEXT_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| "statement identity space exhausted".to_string())?;
        Ok(Resolved {
            id,
            template,
            definitions: definitions.into_boxed_slice(),
        })
    }
}

/// Rewrite a compiled template to use recorded names. The macro emits
/// `refs` and `@` placeholders in the same order as the attribute list.
fn rename(
    encoded: &[u8],
    attrs: BTreeMap<String, String>,
    names: &[&str],
) -> Result<Vec<u8>, String> {
    let invalid = |error: &dyn std::fmt::Display| format!("invalid compiled template: {error}");
    let mut template: StatementTemplate =
        serde_json::from_slice(encoded).map_err(|error| invalid(&error))?;
    template.attrs = attrs;
    if template.refs.len() != names.len() {
        return Err(invalid(&"reference count differs from attributes"));
    }
    for (reference, name) in template.refs.iter_mut().zip(names) {
        let binding = std::mem::take(reference)
            .into_values()
            .next()
            .ok_or_else(|| invalid(&"empty reference"))?;
        reference.insert((*name).into(), binding);
    }
    let mut parts = format::parse(&template.msg).map_err(|error| invalid(&error))?;
    let mut names = names.iter();
    for part in &mut parts {
        if let Part::Field(field) = part
            && field.name.starts_with('@')
        {
            let name = names
                .next()
                .ok_or_else(|| invalid(&"placeholder count differs from attributes"))?;
            field.name = format!("@{name}");
        }
    }
    template.msg = format::unparse(&parts);
    serde_json::to_vec(&template).map_err(|error| invalid(&error))
}

#[doc(hidden)]
pub const fn encoded_len(parts: &[&[u8]]) -> usize {
    let mut length = 0;
    let mut index = 0;
    while index < parts.len() {
        length += parts[index].len();
        index += 1;
    }
    length
}

// Compiler-provided file and module strings are UTF-8 but are not JSON literals
// (e.g. backslashes in a Windows path). Escape them during const evaluation.
const fn json_escape_len(b: u8) -> usize {
    match b {
        b'"' | b'\\' => 2,
        0..=31 => 6,
        _ => 1,
    }
}

#[doc(hidden)]
pub const fn json_string_len(text: &str) -> usize {
    let mut len = 2;
    let mut i = 0;
    while i < text.len() {
        len += json_escape_len(text.as_bytes()[i]);
        i += 1;
    }
    len
}

#[doc(hidden)]
pub const fn json_string<const N: usize>(text: &str) -> [u8; N] {
    let mut out = [b'"'; N];
    let (mut i, mut pos) = (0, 1);
    let hex = b"0123456789abcdef";
    while i < text.len() {
        let b = text.as_bytes()[i];
        match b {
            b'"' | b'\\' => {
                out[pos] = b'\\';
                out[pos + 1] = b;
            }
            0..=31 => {
                out[pos] = b'\\';
                out[pos + 1] = b'u';
                out[pos + 2] = b'0';
                out[pos + 3] = b'0';
                out[pos + 4] = hex[(b >> 4) as usize];
                out[pos + 5] = hex[(b & 15) as usize];
            }
            _ => out[pos] = b,
        }
        pos += json_escape_len(b);
        i += 1;
    }
    assert!(pos + 1 == N);
    out
}

#[doc(hidden)]
pub const fn concatenate<const N: usize>(parts: &[&[u8]]) -> [u8; N] {
    let mut output = [0; N];
    let mut offset = 0;
    let mut part = 0;
    while part < parts.len() {
        let mut byte = 0;
        while byte < parts[part].len() {
            output[offset] = parts[part][byte];
            offset += 1;
            byte += 1;
        }
        part += 1;
    }
    assert!(offset == N);
    output
}

#[doc(hidden)]
pub fn owned_message(
    statement: &'static Statement,
    stack: io::Result<&[u64]>,
    args: &[BorrowedArgument<'_>],
) -> Message {
    let stack = stack.expect("cannot capture logging snapshot stack");
    let timestamp = crate::timestamp_now().expect("cannot timestamp logging snapshot");
    let resolved = statement
        .resolve()
        .unwrap_or_else(|error| panic!("cannot resolve logging statement: {error}"));
    let runtime = runtime();
    let mut defs: BTreeMap<_, _> = resolved
        .definitions
        .iter()
        .map(|&index| {
            let encoded = &runtime.definitions[index];
            (encoded.key.clone(), encoded.definition.clone())
        })
        .collect();
    defs.insert(
        format!("{STATEMENT_PREFIX}{}", resolved.id),
        Definition::Statement(
            serde_json::from_slice(&resolved.template).expect("compiled statement template"),
        ),
    );
    for arg in args {
        if let BorrowedArgument::Entity(entity) = arg {
            let relation = &runtime.definitions[entity.relation];
            defs.entry(relation.key.clone())
                .or_insert_with(|| relation.definition.clone());
            defs.entry(entity.key.clone())
                .or_insert_with(|| Definition::Entity(entity.object.clone()));
        }
    }
    Message {
        timestamp,
        stack: stack.to_vec(),
        attrs: BTreeMap::from([(STATEMENT.into(), resolved.id)]),
        args: args.iter().map(|argument| argument.snapshot()).collect(),
        defs,
    }
}
