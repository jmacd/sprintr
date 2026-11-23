// SPDX-License-Identifier: Apache-2.0

use crate::{RelationType, format::FormatError, reference};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser};
use std::{collections::BTreeMap, fmt};

/// A positional value. Entity definitions are opaque JSON objects, but
/// literal objects and arrays in `args` recursively obey this grammar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Argument {
    String(String),
    Unsigned(u64),
    Reference {
        #[serde(rename = "$ref")]
        target: String,
    },
    Object(#[serde(serialize_with = "serialize_literal")] BTreeMap<String, Argument>),
    Array(Vec<Argument>),
}

fn serialize_literal<S: Serializer>(
    object: &BTreeMap<String, Argument>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    if object.contains_key("$ref") {
        return Err(ser::Error::custom("$ref is reserved for inline references"));
    }
    object.serialize(serializer)
}

impl Argument {
    pub(super) fn check_attribute(&self, name: &str, ty: &RelationType) -> Result<(), FormatError> {
        match (self, ty) {
            (Self::String(_), RelationType::String) | (Self::Unsigned(_), RelationType::U64) => {
                Ok(())
            }
            (Self::Reference { target }, RelationType::Entity(relation))
                if matches!(reference::parse(target)?, reference::Target::Definition { key, .. }
                    if key.split_once(':').is_some_and(|(r, _)| r == relation)) =>
            {
                Ok(())
            }
            _ => Err(FormatError(format!(
                "argument for {name:?} has the wrong type"
            ))),
        }
    }

    pub(super) fn at<'a>(args: &'a [Self], tokens: &[String]) -> Option<&'a Self> {
        let (first, rest) = tokens.split_first()?;
        let mut arg = args.get(reference::index(first)?)?;
        for token in rest {
            arg = match arg {
                Self::Object(object) => object.get(token)?,
                Self::Array(array) => array.get(reference::index(token)?)?,
                _ => return None,
            };
        }
        Some(arg)
    }
}

impl<'de> Deserialize<'de> for Argument {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Argument;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string, unsigned integer, reference, literal object or array")
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Argument, E> {
                Ok(Argument::Unsigned(value))
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Argument, E> {
                self.visit_u64(value.try_into().map_err(E::custom)?)
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Argument, E> {
                self.visit_string(value.to_owned())
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<Argument, E> {
                Ok(Argument::String(value))
            }

            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Argument, A::Error> {
                let mut array = Vec::new();
                while let Some(value) = seq.next_element()? {
                    array.push(value);
                }
                Ok(Argument::Array(array))
            }

            fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Argument, A::Error> {
                let mut object = BTreeMap::new();
                while let Some(key) = map.next_key::<String>()? {
                    if key == "$ref" {
                        let target = map.next_value()?;
                        if !object.is_empty() || map.next_key::<de::IgnoredAny>()?.is_some() {
                            return Err(de::Error::custom("$ref must be the only object member"));
                        }
                        return Ok(Argument::Reference { target });
                    }
                    if object.insert(key, map.next_value()?).is_some() {
                        return Err(de::Error::custom("duplicate literal object member"));
                    }
                }
                Ok(Argument::Object(object))
            }
        }
        d.deserialize_any(Visitor)
    }
}
