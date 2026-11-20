// SPDX-License-Identifier: Apache-2.0

//! The standalone prototype's message representation and format grammar.

pub mod format;

use format::{FormatError, Part, Spec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Wire key prefix for definitions described by the bootstrap relation.
pub const RELATION_PREFIX: &str = "log.relation:";

/// Relation kinds supported by the current prototype.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RelationKind {
    /// An attribute associating a name with an argument value.
    Attribute,
}

/// Value types supported by registered attributes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RelationType {
    /// String values, captured through `AsRef<str>`.
    String,
}

/// A self-describing attribute definition under `log.relation`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationDefinition {
    pub kind: RelationKind,
    #[serde(rename = "type")]
    pub value_type: RelationType,
    pub desc: String,
}

/// A captured value, retained as its native JSON type rather than formatted text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Argument {
    /// String data, including any literal braces or newlines.
    String(String),
    /// An unsigned integer, with the full `u64` range preserved.
    Unsigned(u64),
}

/// Borrowed arguments used by generated direct-encoding code.
#[doc(hidden)]
#[derive(Serialize)]
#[serde(untagged)]
pub enum BorrowedArgument<'a> {
    String(&'a str),
    Unsigned(u64),
}

/// One JSON Lines record, with optional relation definitions and associations.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[must_use = "a captured message must be recorded or rendered"]
pub struct Message {
    /// The stored format, with source labels and rendering specifiers.
    pub msg: String,
    /// Arguments in placeholder occurrence order.
    pub args: Vec<Argument>,
    /// Relation definitions included in this record.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub defs: BTreeMap<String, RelationDefinition>,
    /// Ordered singleton maps associating names with local argument pointers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<BTreeMap<String, String>>,
}

impl Message {
    /// Check format syntax, argument types/counts, and local reference structure.
    ///
    /// Relation availability and stream-wide conflicts are checked by [`Renderer`].
    pub fn validate(&self) -> Result<(), FormatError> {
        self.validate_parts(&format::parse(&self.msg)?)?;
        self.validate_metadata()
    }

    fn validate_metadata(&self) -> Result<(), FormatError> {
        for key in self.defs.keys() {
            relation_name(key)?;
        }
        for reference in &self.refs {
            if reference.len() != 1 {
                return Err(FormatError(
                    "each refs entry must contain exactly one name and pointer".into(),
                ));
            }
            let (name, pointer) = reference.iter().next().expect("one reference");
            if name.is_empty() {
                return Err(FormatError("reference names must not be empty".into()));
            }
            let index = argument_index(pointer)?;
            if index >= self.args.len() {
                return Err(FormatError(format!(
                    "reference {pointer:?} is outside args"
                )));
            }
        }
        Ok(())
    }

    fn validate_parts(&self, parts: &[Part]) -> Result<(), FormatError> {
        let fields = parts.iter().filter_map(|part| match part {
            Part::Field(field) => Some(field),
            Part::Literal(_) => None,
        });
        let count = fields.clone().count();
        if count != self.args.len() {
            return Err(FormatError(format!(
                "format has {count} placeholders but record has {} arguments",
                self.args.len()
            )));
        }
        for (index, (field, argument)) in fields.zip(&self.args).enumerate() {
            if !matches!(
                (field.spec, argument),
                (Spec::String, Argument::String(_)) | (Spec::Hex { .. }, Argument::Unsigned(_))
            ) {
                return Err(FormatError(format!(
                    "argument {index} ({}) must be {}",
                    field.name,
                    match field.spec {
                        Spec::String => "a string",
                        Spec::Hex { .. } => "an unsigned integer",
                    }
                )));
            }
        }
        Ok(())
    }

    /// Render a self-contained record, followed by its positional associations.
    ///
    /// Use [`Renderer`] when later records refer to definitions from earlier ones.
    pub fn render(&self) -> Result<String, FormatError> {
        Renderer::default().render(self)
    }

    fn render_text(&self) -> Result<String, FormatError> {
        let parts = format::parse(&self.msg)?;
        self.validate_parts(&parts)?;
        let mut arguments = self.args.iter();
        let mut output = String::new();
        for part in parts {
            match part {
                Part::Literal(text) => output.push_str(&text),
                Part::Field(field) => match (field.spec, arguments.next()) {
                    (Spec::String, Some(Argument::String(value))) => output.push_str(value),
                    (
                        Spec::Hex {
                            alternate,
                            zero_pad,
                            width,
                        },
                        Some(Argument::Unsigned(value)),
                    ) => {
                        let text = match (alternate, zero_pad) {
                            (true, true) => format!("{value:#0width$x}"),
                            (true, false) => format!("{value:#width$x}"),
                            (false, true) => format!("{value:0width$x}"),
                            (false, false) => format!("{value:width$x}"),
                        };
                        output.push_str(&text);
                    }
                    _ => unreachable!("argument count and types were validated"),
                },
            }
        }
        Ok(output)
    }
}

fn relation_name(key: &str) -> Result<&str, FormatError> {
    match key.strip_prefix(RELATION_PREFIX) {
        Some(name) if !name.is_empty() && name != "log.relation" => Ok(name),
        _ => Err(FormatError(format!(
            "unsupported relation definition {key:?}"
        ))),
    }
}

fn argument_index(pointer: &str) -> Result<usize, FormatError> {
    let invalid = || FormatError(format!("unsupported argument reference {pointer:?}"));
    let index = pointer.strip_prefix("#/args/").ok_or_else(invalid)?;
    if index.is_empty()
        || !index.bytes().all(|byte| byte.is_ascii_digit())
        || (index.len() > 1 && index.starts_with('0'))
    {
        return Err(invalid());
    }
    index.parse().map_err(|_| invalid())
}

/// Relation-aware playback for one process's record stream.
#[derive(Default)]
pub struct Renderer {
    relations: BTreeMap<String, RelationDefinition>,
}

impl Renderer {
    /// Load definitions and render text plus a second line of positional associations.
    ///
    /// Conflicts and unresolved references are errors. Invalid records do not
    /// change the renderer's definition table.
    pub fn render(&mut self, message: &Message) -> Result<String, FormatError> {
        message.validate_metadata()?;
        let mut output = message.render_text()?;
        for (key, definition) in &message.defs {
            let name = relation_name(key)?;
            if self
                .relations
                .get(name)
                .is_some_and(|existing| existing != definition)
            {
                return Err(FormatError(format!("conflicting definition for {name:?}")));
            }
        }
        for (position, reference) in message.refs.iter().enumerate() {
            let (name, pointer) = reference.iter().next().expect("validated reference");
            let key = format!("{RELATION_PREFIX}{name}");
            let definition = message
                .defs
                .get(&key)
                .or_else(|| self.relations.get(name))
                .ok_or_else(|| FormatError(format!("unknown relation {name:?}")))?;
            let index = argument_index(pointer)?;
            let value = match (&definition.value_type, &message.args[index]) {
                (RelationType::String, Argument::String(value)) => value,
                _ => {
                    return Err(FormatError(format!(
                        "attribute {name:?} requires a string argument"
                    )));
                }
            };
            output.push_str(if position == 0 { "\n" } else { ", " });
            output.extend(name.escape_debug());
            output.push_str(" = ");
            output.extend(value.escape_debug());
        }
        for (key, definition) in &message.defs {
            let name = relation_name(key)?;
            if !self.relations.contains_key(name) {
                self.relations.insert(name.into(), definition.clone());
            }
        }
        Ok(output)
    }
}
