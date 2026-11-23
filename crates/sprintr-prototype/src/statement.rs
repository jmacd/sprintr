// SPDX-License-Identifier: Apache-2.0

use crate::StatementTemplate;
use crate::declare::{Named, RelationMeta};
use crate::format::{self, Part};
use crate::runtime::runtime;
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, hash_map::Entry};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Compiler-generated anonymous entity, unique to a macro callsite.
#[doc(hidden)]
pub struct Statement {
    encoded: &'static [u8],
    attributes: &'static [&'static RelationMeta],
    values: &'static [&'static Named],
    resolved: OnceLock<Result<Resolved, String>>,
}

/// A `structured!` invocation shared by every concrete type using this callsite.
#[doc(hidden)]
pub struct NestedTemplate {
    encoded: &'static [u8],
    attributes: &'static [(usize, &'static RelationMeta)],
    file: &'static str,
    line: u32,
    resolved: OnceLock<Result<NestedFormat, String>>,
}

pub(crate) struct NestedFormat {
    pub msg: String,
    pub bindings: Vec<(usize, usize)>,
}

impl NestedTemplate {
    pub const fn new(
        encoded: &'static [u8],
        attributes: &'static [(usize, &'static RelationMeta)],
        file: &'static str,
        line: u32,
    ) -> Self {
        Self {
            encoded,
            attributes,
            file,
            line,
            resolved: OnceLock::new(),
        }
    }

    pub(crate) fn resolve(&self) -> io::Result<&NestedFormat> {
        self.resolved
            .get_or_init(|| {
                let mut msg: String =
                    serde_json::from_slice(self.encoded).map_err(|e| e.to_string())?;
                let mut bindings = Vec::new();
                let mut names = Vec::new();
                let mut renamed = false;
                for &(argument, attr) in self.attributes {
                    let relation = runtime().relation(attr)?;
                    let name = runtime().definitions[relation].name.as_str();
                    renamed |= name != attr.name;
                    names.push(name);
                    bindings.push((argument, relation));
                }
                if renamed {
                    msg = rename_fields(&msg, &names)?;
                }
                Ok(NestedFormat { msg, bindings })
            })
            .as_ref()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.clone()))
    }
}

pub(crate) struct NamedEncoder {
    pub key: String,
    pub member: Vec<u8>,
    pub id: usize,
}

// A static inside a generic impl is shared by all monomorphizations.
type Variant = (usize, &'static str);

#[derive(Default)]
pub(crate) struct EncoderCache(HashMap<Variant, Arc<NamedEncoder>>);

impl EncoderCache {
    pub fn resolve(
        &mut self,
        site: &'static NestedTemplate,
        name: &'static str,
        msg: &str,
    ) -> io::Result<&Arc<NamedEncoder>> {
        let variant = (std::ptr::from_ref(site).addr(), name);
        match self.0.entry(variant) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => Ok(entry.insert(intern_encoder(variant, site, name, msg)?)),
        }
    }
}

// Cold path only. Definitions are immutable and can be copied to any output;
// whether a particular thread has delivered one is tracked by that output.
fn intern_encoder(
    variant: Variant,
    site: &NestedTemplate,
    name: &str,
    msg: &str,
) -> io::Result<Arc<NamedEncoder>> {
    #[derive(Default)]
    struct Registry {
        variants: HashMap<Variant, Arc<NamedEncoder>>,
        names: HashMap<String, Arc<NamedEncoder>>,
    }
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    let mut registry = REGISTRY
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| io::Error::other("encoder registry poisoned"))?;
    if let Some(known) = registry.variants.get(&variant) {
        return Ok(known.clone());
    }
    let def = sprintr_message::EncodeDefinition {
        msg: msg.into(),
        name: name.into(),
        file: site.file.into(),
        line: site.line,
    };
    let bytes = serde_json::to_vec(&def).map_err(crate::json_error)?;
    let hash = crate::runtime::fnv1a(&bytes);
    let leaf = crate::rust_leaf_name(name).unwrap_or("Value");
    let key = format!(
        "{}{leaf}_{hash:016x}",
        sprintr_message::STRUCT_ENCODE_PREFIX
    );
    let mut member = serde_json::to_vec(&key).map_err(crate::json_error)?;
    member.push(b':');
    member.extend_from_slice(&bytes);
    let id = registry.names.len();
    let encoded = match registry.names.entry(key.clone()) {
        Entry::Occupied(entry) if entry.get().member == member => entry.get().clone(),
        Entry::Occupied(_) => {
            return Err(io::Error::other(format!(
                "conflicting encoder identity {key}"
            )));
        }
        Entry::Vacant(entry) => entry
            .insert(Arc::new(NamedEncoder { key, member, id }))
            .clone(),
    };
    registry.variants.insert(variant, encoded.clone());
    Ok(encoded)
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
    template.msg = rename_fields(&template.msg, names)?;
    serde_json::to_vec(&template).map_err(|error| invalid(&error))
}

fn rename_fields(msg: &str, names: &[&str]) -> Result<String, String> {
    let mut parts = format::parse(msg).map_err(|e| e.to_string())?;
    let mut names = names.iter();
    for part in &mut parts {
        if let Part::Field(field) = part
            && field.name.starts_with('@')
        {
            let name = names
                .next()
                .ok_or("placeholder count differs from attributes")?;
            field.name = format!("@{name}");
        }
    }
    Ok(format::unparse(&parts))
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
