// SPDX-License-Identifier: Apache-2.0

//! Process-wide resolution of declared names, performed once at first use.

use crate::declare::{Declaration, Named, RelationMeta};
use crate::{Definition, RELATION_PREFIX};
use std::collections::HashMap;
use std::sync::OnceLock;

/// One definition, pre-encoded as a `"key":value` member of a `defs` object.
pub(crate) struct Encoded {
    /// The recorded name: the key without its relation prefix.
    pub(crate) name: String,
    pub(crate) bytes: Box<[u8]>,
    pub(crate) definition: Definition,
}

/// A declaration recorded under a new name because an earlier one differs.
pub(crate) struct Collision {
    pub(crate) kind: &'static str,
    pub(crate) declared: String,
    pub(crate) description: &'static str,
    pub(crate) recorded: String,
}

#[derive(Default)]
pub(crate) struct Runtime {
    pub(crate) definitions: Vec<Encoded>,
    pub(crate) collisions: Vec<Collision>,
    keys: HashMap<String, usize>,
    /// Declarations by address. Distinct statics have distinct addresses.
    relations: HashMap<usize, usize>,
    named: HashMap<usize, usize>,
}

pub(crate) fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        // sprintr's schema comes first so that its names are never renamed.
        Runtime::new(
            crate::schema::DECLARATIONS
                .iter()
                .chain(crate::__private::DECLARATIONS.iter()),
        )
    })
}

fn address<T>(value: &'static T) -> usize {
    value as *const T as usize
}

pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

impl Runtime {
    pub(crate) fn new<'a>(declarations: impl Iterator<Item = &'a Declaration> + Clone) -> Self {
        let mut runtime = Self::default();
        for declaration in declarations.clone() {
            if let Declaration::Relation(meta) = declaration {
                runtime.declare_relation(meta);
            }
        }
        for declaration in declarations {
            if let Declaration::Named(named) = declaration {
                runtime.declare_named(named);
            }
        }
        runtime
    }

    fn declare_relation(&mut self, meta: &'static RelationMeta) -> usize {
        if let Some(index) = self.relations.get(&address(meta)) {
            return *index;
        }
        let definition = meta.definition(|target| {
            let index = self.declare_relation(target);
            self.definitions[index].name.clone()
        });
        let index = self.insert(
            RELATION_PREFIX,
            meta.name,
            Definition::Relation(definition),
            meta.kind_name(),
            meta.description,
        );
        self.relations.insert(address(meta), index);
        index
    }

    fn declare_named(&mut self, named: &'static Named) -> usize {
        if let Some(index) = self.named.get(&address(named)) {
            return *index;
        }
        let relation = self.declare_relation(named.relation().__meta());
        let prefix = format!("{}:", self.definitions[relation].name);
        let index = self.insert(
            &prefix,
            named.name(),
            Definition::Entity(
                [("desc".into(), named.description().into())]
                    .into_iter()
                    .collect(),
            ),
            "named entity",
            named.description(),
        );
        self.named.insert(address(named), index);
        index
    }

    /// The first definition of a key keeps its name. A later, different
    /// definition is renamed with a suffix derived from its content, so equal
    /// declarations in different crates resolve to the same renamed key.
    fn insert(
        &mut self,
        prefix: &str,
        name: &str,
        definition: Definition,
        kind: &'static str,
        description: &'static str,
    ) -> usize {
        let value = serde_json::to_string(&definition).expect("definition JSON encoding");
        let hash = fnv1a(value.as_bytes());
        let shown = |name: &str| {
            if prefix == RELATION_PREFIX {
                name.to_string()
            } else {
                format!("{prefix}{name}")
            }
        };
        let mut candidate = name.to_string();
        let mut attempt = 0;
        loop {
            let key = format!("{prefix}{candidate}");
            match self.keys.get(&key) {
                Some(&index) if self.definitions[index].definition == definition => return index,
                Some(_) => {
                    attempt += 1;
                    candidate = match attempt {
                        1 => format!("{name}_{:08x}", hash & 0xffff_ffff),
                        2 => format!("{name}_{hash:016x}"),
                        _ => format!("{name}_{hash:016x}_{attempt}"),
                    };
                }
                None => {
                    if attempt > 0 {
                        self.collisions.push(Collision {
                            kind,
                            declared: shown(name),
                            description,
                            recorded: shown(&candidate),
                        });
                    }
                    let bytes = format!("{}:{value}", serde_json::to_string(&key).expect("key"));
                    let index = self.definitions.len();
                    self.keys.insert(key, index);
                    self.definitions.push(Encoded {
                        name: candidate,
                        bytes: bytes.into_bytes().into_boxed_slice(),
                        definition,
                    });
                    return index;
                }
            }
        }
    }

    pub(crate) fn relation(&self, meta: &'static RelationMeta) -> Result<usize, String> {
        self.relations
            .get(&address(meta))
            .copied()
            .ok_or_else(|| format!("relation {:?} was not registered at startup", meta.name))
    }

    pub(crate) fn named(&self, named: &'static Named) -> Result<usize, String> {
        self.named.get(&address(named)).copied().ok_or_else(|| {
            format!(
                "named entity {}:{} was not registered at startup",
                named.relation().name(),
                named.name()
            )
        })
    }
}
