// SPDX-License-Identifier: Apache-2.0

use crate::{
    Argument, Attribute, Definition, Level, Message, NamedEntity, RelationDefinition, RelationKind,
    RelationType,
};
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

static ATTRIBUTES: Mutex<Option<HashMap<&'static str, &'static str>>> = Mutex::new(None);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Compiler-generated anonymous entity, unique to a macro callsite.
#[doc(hidden)]
pub struct Statement {
    pub(crate) encoded: &'static [u8],
    pub(crate) attributes: &'static [&'static Attribute],
    pub(crate) level: Option<Level>,
    pub(crate) level_definition: &'static [u8],
    id: OnceLock<Result<u64, String>>,
}

impl Statement {
    pub const fn new(
        encoded: &'static [u8],
        attributes: &'static [&'static Attribute],
        level: Option<Level>,
        level_definition: &'static [u8],
    ) -> Self {
        Self {
            encoded,
            attributes,
            level,
            level_definition,
            id: OnceLock::new(),
        }
    }

    pub(crate) fn id(&self) -> io::Result<u64> {
        self.id
            .get_or_init(|| {
                let mut registry = ATTRIBUTES
                    .lock()
                    .map_err(|_| "attribute registry is poisoned".to_string())?;
                let registry = registry.get_or_insert_with(HashMap::new);
                for (index, attribute) in self.attributes.iter().enumerate() {
                    if registry
                        .get(attribute.name)
                        .is_some_and(|desc| *desc != attribute.description)
                        || self.attributes[..index].iter().any(|previous| {
                            previous.name == attribute.name
                                && previous.description != attribute.description
                        })
                    {
                        return Err(format!("conflicting definition for {:?}", attribute.name));
                    }
                }
                let id = NEXT_ID
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                    .map_err(|_| "statement identity space exhausted".to_string())?;
                for attribute in self.attributes {
                    registry.insert(attribute.name, attribute.description);
                }
                Ok(id)
            })
            .as_ref()
            .copied()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.clone()))
    }
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
pub fn owned_message(statement: &'static Statement, args: Vec<Argument>) -> Message {
    let id = statement.id().expect("invalid static logging schema");
    let mut defs = BTreeMap::from([
        (
            "log.relation:log.statement".into(),
            Definition::Relation(RelationDefinition::statement()),
        ),
        (
            format!("log.statement:{id}"),
            Definition::Statement(
                serde_json::from_slice(statement.encoded)
                    .expect("compiler-generated statement template"),
            ),
        ),
    ]);
    if let Some(level) = statement.level {
        defs.insert(
            "log.relation:log.level".into(),
            Definition::Relation(RelationDefinition::level()),
        );
        defs.insert(
            format!("log.level:{}", level.name()),
            Definition::NamedEntity(NamedEntity {
                desc: level.description().into(),
            }),
        );
    }
    for attribute in statement.attributes {
        defs.entry(format!("log.relation:{}", attribute.name))
            .or_insert_with(|| {
                Definition::Relation(RelationDefinition {
                    kind: RelationKind::Attribute,
                    value_type: RelationType::String,
                    desc: attribute.description.into(),
                    anonymous: false,
                })
            });
    }
    Message {
        attrs: BTreeMap::from([("log.statement".into(), id)]),
        args,
        defs,
    }
}
