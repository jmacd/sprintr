// SPDX-License-Identifier: Apache-2.0

use super::{
    RECORD, RelationDefinition, RelationKind, RelationType, Renderer, SPAN_PARENT, SPAN_START,
    STRUCT_ENCODE, STRUCT_NAME, TRACE_START, format::FormatError, reference, span,
    validate_entity_name, validate_relation_name,
};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::collections::{BTreeMap, BTreeSet};

/// Scope declared for a named entity's own ID (not necessarily its outermost ID).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdScope {
    Global,
    Local,
}

/// A literal scalar in an identity. A numeric string is not an unsigned ID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IdentityValue {
    String(String),
    Unsigned(u64),
}

impl From<&str> for IdentityValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}
impl From<String> for IdentityValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}
macro_rules! unsigned_values {
    ($($t:ty),*) => { $(impl From<$t> for IdentityValue {
        fn from(value: $t) -> Self { Self::Unsigned(u64::from(value)) }
    })* };
}
unsigned_values!(u8, u16, u32, u64);

/// One level of a qualified identity, interpreted using its entity relation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityComponent {
    pub entity_type: String,
    /// In `id_keys` order; a static name or anonymous UID occupies a single slot.
    pub values: Vec<IdentityValue>,
}

pub(super) fn id_keys<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let keys = Vec::<String>::deserialize(d)?;
    if keys.is_empty() {
        return Err(de::Error::custom(
            "omit id_keys when there are no ID attributes",
        ));
    }
    Ok(keys)
}

impl RelationDefinition {
    pub(super) fn validate_identity(&self, name: &str) -> Result<(), FormatError> {
        // Old named declarations say nothing about scope. Do not infer Global.
        if self.id_scope.is_none() && self.scope_entity_type.is_none() && self.id_keys.is_empty() {
            return Ok(());
        }
        if !self.describes_named_entities() {
            return Err(FormatError(format!(
                "only named entities may declare identity metadata: {name:?}"
            )));
        }
        match (self.id_scope, self.scope_entity_type.as_deref()) {
            (Some(IdScope::Global), None) => {}
            (Some(IdScope::Local), Some(parent)) => validate_relation_name(parent)?,
            _ => {
                return Err(FormatError(format!(
                    "inconsistent identity scope for {name:?}"
                )));
            }
        }
        let valid = match name {
            SPAN_START | SPAN_PARENT => {
                self.id_keys == ["span.id"]
                    && self.scope_entity_type.as_deref() == Some(TRACE_START)
            }
            TRACE_START => self.id_keys == ["trace.id"] && self.id_scope == Some(IdScope::Global),
            STRUCT_ENCODE | STRUCT_NAME | super::EXCEPTION_CLASS | "log.level" | "log.kind" => {
                self.id_keys.is_empty() && self.id_scope == Some(IdScope::Global)
            }
            _ => self.id_scope == Some(IdScope::Global) || !self.id_keys.is_empty(),
        };
        if !valid {
            return Err(FormatError(format!(
                "invalid identity declaration for {name:?}"
            )));
        }
        let mut distinct = BTreeSet::new();
        for key in &self.id_keys {
            validate_relation_name(key)?;
            if !distinct.insert(key) {
                return Err(FormatError(format!(
                    "duplicate ID attribute {key:?} in {name:?}"
                )));
            }
        }
        Ok(())
    }
}

/// Validate pending declarations against both pending and already accepted schema.
pub(super) fn validate_relations<'a>(
    pending: &BTreeMap<String, RelationDefinition>,
    relation: impl Fn(&str) -> Option<&'a RelationDefinition>,
) -> Result<(), FormatError> {
    for (name, def) in pending {
        if let RelationType::Entity(target) = &def.value_type
            && !relation(target).is_some_and(|r| r.kind == RelationKind::Entity)
        {
            return Err(FormatError(format!(
                "expected an entity relation for attribute target {target:?}"
            )));
        }
        for key in &def.id_keys {
            let Some(RelationDefinition {
                kind: RelationKind::Attribute,
                value_type: ty @ (RelationType::String | RelationType::U64),
                ..
            }) = relation(key)
            else {
                return Err(FormatError(format!(
                    "ID attribute {key:?} of {name:?} must have type string or u64"
                )));
            };
            if matches!(name.as_str(), SPAN_START | SPAN_PARENT | TRACE_START)
                && *ty != RelationType::String
            {
                return Err(FormatError(format!(
                    "{name:?} needs a string ID attribute {key:?}"
                )));
            }
        }
        if let Some(parent) = &def.scope_entity_type
            && !relation(parent).is_some_and(|r| {
                r.kind == RelationKind::Entity && (r.anonymous || r.id_scope.is_some())
            })
        {
            return Err(FormatError(format!(
                "scope {parent:?} of {name:?} needs an entity relation with a known identity"
            )));
        }
    }

    // An accepted declaration cannot point back into pending. Walk each new
    // scope edge once, without recursion on producer-controlled chain depth.
    let mut finished = BTreeSet::new();
    for name in pending.keys() {
        let mut path = BTreeSet::new();
        let mut node = name.as_str();
        while let Some(parent) = pending
            .get(node)
            .and_then(|d| d.scope_entity_type.as_deref())
        {
            if finished.contains(node) {
                break;
            }
            if !path.insert(node) {
                return Err(FormatError(format!("cycle in entity scope at {node:?}")));
            }
            node = parent;
        }
        finished.extend(path);
    }
    Ok(())
}

/// Walk the already validated schema from leaf to root, consuming exactly one
/// component per declaration. No parent instance lookup or address is involved.
pub(super) fn read_runtime_identity<'a>(
    mut name: &'a str,
    mut def: &'a RelationDefinition,
    suffix: &str,
    relation: impl Fn(&str) -> Option<&'a RelationDefinition>,
    mut component: impl FnMut(&str, Vec<IdentityValue>),
) -> Result<(), FormatError> {
    let parts: Vec<Vec<IdentityValue>> = serde_json::from_str(suffix)
        .map_err(|e| FormatError(format!("invalid identity for {name:?}: {e}")))?;
    if serde_json::to_string(&parts).expect("identity scalars serialize") != suffix {
        return Err(FormatError(format!("noncanonical identity for {name:?}")));
    }
    let mut parts = parts.into_iter().rev();
    loop {
        let values = parts
            .next()
            .ok_or_else(|| FormatError(format!("missing identity component for {name:?}")))?;
        let valid = if !def.id_keys.is_empty() {
            values.len() == def.id_keys.len()
                && def.id_keys.iter().zip(&values).all(|(key, value)| {
                    matches!(
                        (relation(key).map(|r| &r.value_type), value),
                        (Some(RelationType::String), IdentityValue::String(_))
                            | (Some(RelationType::U64), IdentityValue::Unsigned(_))
                    )
                })
        } else if def.anonymous {
            matches!(&values[..], [IdentityValue::Unsigned(uid)] if *uid != 0)
        } else {
            matches!(&values[..], [IdentityValue::String(id)] if !id.is_empty())
        };
        if !valid {
            return Err(FormatError(format!(
                "invalid identity component for {name:?}"
            )));
        }
        if let [IdentityValue::String(id)] = &values[..] {
            // A marker can enclose an ordinary entity without having a defs entry.
            span::marker_id(name, id)?;
        }
        component(name, values);
        let Some(parent) = def.scope_entity_type.as_deref() else {
            break;
        };
        name = parent;
        def = relation(parent).expect("validated scope relation");
    }
    if parts.next().is_some() {
        return Err(FormatError(format!(
            "extra identity component outside {name:?}"
        )));
    }
    Ok(())
}

impl Renderer {
    /// Decode an entity definition key into typed components, outermost first.
    /// Only accepted relation schemas are needed, not any instance definitions.
    /// Consult [`Self::relation`] for each component's scope and key attributes;
    /// a legacy named relation has no declared scope. Span annotations, unlike
    /// definitions, carry their trace ID separately (see [`crate::Message::span_links`]).
    pub fn identity_components(&self, key: &str) -> Result<Vec<IdentityComponent>, FormatError> {
        let (name, suffix) = key
            .split_once(':')
            .ok_or_else(|| FormatError(format!("expected an entity key, got {key:?}")))?;
        let def = self
            .relation(name)
            .filter(|r| {
                r.kind == RelationKind::Entity
                    && !matches!(name, RECORD | TRACE_START | SPAN_START | SPAN_PARENT)
            })
            .ok_or_else(|| {
                FormatError(format!(
                    "{name:?} is not a definition-bearing entity relation"
                ))
            })?;
        if !def.id_keys.is_empty() {
            let mut components = Vec::new();
            read_runtime_identity(
                name,
                def,
                suffix,
                |r| self.relation(r),
                |entity_type, values| {
                    components.push(IdentityComponent {
                        entity_type: entity_type.into(),
                        values,
                    });
                },
            )?;
            components.reverse();
            return Ok(components);
        }
        let value =
            if def.anonymous {
                IdentityValue::Unsigned(reference::positive_id(suffix).ok_or_else(|| {
                    FormatError(format!("invalid anonymous entity identity {key:?}"))
                })?)
            } else {
                validate_entity_name(suffix)?;
                IdentityValue::String(suffix.to_owned())
            };
        Ok(vec![IdentityComponent {
            entity_type: name.to_owned(),
            values: vec![value],
        }])
    }
}
