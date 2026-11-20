// SPDX-License-Identifier: Apache-2.0

//! Process-wide registration and cached encoding of string attribute relations.

use slogr_message::{
    Argument, Message, RELATION_PREFIX, RelationDefinition, RelationKind, RelationType,
};
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::{Arc, Mutex, OnceLock};

static ATTRIBUTES: OnceLock<Mutex<HashMap<Arc<str>, Arc<Registered>>>> = OnceLock::new();

/// A registered string attribute, borrowed at logging callsites.
#[derive(Clone)]
pub struct Attribute {
    pub(crate) relation: Arc<Registered>,
}

pub(crate) struct Registered {
    pub id: usize,
    pub name: Arc<str>,
    pub key: String,
    pub definition: RelationDefinition,
    pub encoded_definition: Vec<u8>,
    pub reference_prefix: Vec<u8>,
}

impl Attribute {
    /// The canonical name, independent of the Rust variable naming this handle.
    pub fn name(&self) -> &str {
        &self.relation.name
    }

    #[doc(hidden)]
    pub fn __value<'a, V: AsRef<str> + ?Sized>(&self, value: &'a V) -> &'a str {
        value.as_ref()
    }
}

/// Register a string attribute in the process-wide relation namespace.
///
/// Identical registration reuses the same definition. A different description
/// for an existing name is an error. Registration encodes the definition once;
/// it does not emit a record. Each logger includes it on the first successful
/// record using the attribute.
pub fn register_attribute(name: &str, description: &str) -> io::Result<Attribute> {
    if name.is_empty() || name == "log.relation" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "attribute name is empty or reserved",
        ));
    }
    let mut registry = ATTRIBUTES
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| io::Error::other("attribute registry is poisoned"))?;
    if let Some(relation) = registry.get(name) {
        if relation.definition.desc != description {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("conflicting definition for {name:?}"),
            ));
        }
        return Ok(Attribute {
            relation: Arc::clone(relation),
        });
    }
    let key = format!("{RELATION_PREFIX}{name}");
    let definition = RelationDefinition {
        kind: RelationKind::Attribute,
        value_type: RelationType::String,
        desc: description.into(),
    };
    let mut encoded_definition = serde_json::to_vec(&key).map_err(io::Error::other)?;
    encoded_definition.push(b':');
    serde_json::to_writer(&mut encoded_definition, &definition).map_err(io::Error::other)?;
    let mut reference_prefix = vec![b'{'];
    serde_json::to_writer(&mut reference_prefix, name).map_err(io::Error::other)?;
    reference_prefix.extend_from_slice(b":\"#/args/");
    let name: Arc<str> = name.into();
    let relation = Arc::new(Registered {
        id: registry.len(),
        name: Arc::clone(&name),
        key,
        definition,
        encoded_definition,
        reference_prefix,
    });
    registry.insert(name, Arc::clone(&relation));
    Ok(Attribute { relation })
}

#[doc(hidden)]
pub struct AttributeBinding<'a> {
    pub attribute: &'a Attribute,
    pub argument: usize,
}

#[doc(hidden)]
pub fn owned_message(
    format: &str,
    args: Vec<Argument>,
    bindings: &[AttributeBinding<'_>],
) -> Message {
    let mut message = Message {
        msg: format.into(),
        args,
        ..Message::default()
    };
    for binding in bindings {
        let relation = &binding.attribute.relation;
        message
            .defs
            .entry(relation.key.clone())
            .or_insert_with(|| relation.definition.clone());
        message.refs.push(BTreeMap::from([(
            relation.name.to_string(),
            format!("#/args/{}", binding.argument),
        )]));
    }
    message
}
