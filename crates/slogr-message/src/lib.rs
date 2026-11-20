// SPDX-License-Identifier: Apache-2.0

//! Statement templates, event records, and stateful formatted playback.

pub mod format;

use format::{FormatError, Part, Spec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const RELATION_PREFIX: &str = "log.relation:";
pub const STATEMENT_PREFIX: &str = "log.statement:";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RelationKind {
    Attribute,
    Entity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RelationType {
    String,
    Object,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationDefinition {
    pub kind: RelationKind,
    #[serde(rename = "type")]
    pub value_type: RelationType,
    pub desc: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub anonymous: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

impl RelationDefinition {
    pub fn statement() -> Self {
        Self {
            kind: RelationKind::Entity,
            value_type: RelationType::Object,
            desc: "Logging statement".into(),
            anonymous: true,
        }
    }
}

/// Names must also be representable inside a canonical `{@name}` placeholder.
pub fn validate_attribute_name(name: &str) -> Result<(), FormatError> {
    if name.is_empty()
        || matches!(name, "log.relation" | "log.statement")
        || name.contains(['{', '}', ':'])
    {
        return Err(FormatError(
            "attribute name is empty, reserved, or contains braces/colon".into(),
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Argument {
    String(String),
    Unsigned(u64),
}

#[doc(hidden)]
#[derive(Serialize)]
#[serde(untagged)]
pub enum BorrowedArgument<'a> {
    String(&'a str),
    Unsigned(u64),
}

/// A template parameter, not a JSON Pointer into the definition's record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArgumentBinding {
    #[serde(rename = "$arg")]
    pub argument: usize,
}

/// Static metadata shared by every occurrence of a logging statement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatementTemplate {
    pub attrs: BTreeMap<String, u64>,
    pub refs: Vec<BTreeMap<String, ArgumentBinding>>,
    pub msg: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Definition {
    Relation(RelationDefinition),
    Statement(StatementTemplate),
}

/// One occurrence, plus any definitions first needed by this stream.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[must_use = "a captured message must be recorded or rendered"]
pub struct Message {
    pub attrs: BTreeMap<String, u64>,
    pub args: Vec<Argument>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub defs: BTreeMap<String, Definition>,
}

impl Message {
    /// Validate a self-contained snapshot. Stream-dependent events need a [`Renderer`].
    pub fn validate(&self) -> Result<(), FormatError> {
        self.render().map(|_| ())
    }

    pub fn render(&self) -> Result<String, FormatError> {
        Renderer::default().render(self)
    }
}

struct CompiledStatement {
    template: StatementTemplate,
    parts: Vec<Part>,
    bindings: Vec<(String, usize)>,
}

impl CompiledStatement {
    fn compile<'a>(
        template: &StatementTemplate,
        relation: impl Fn(&str) -> Option<&'a RelationDefinition>,
    ) -> Result<Self, FormatError> {
        if !template.attrs.is_empty() {
            return Err(FormatError(
                "static event attributes are not supported yet".into(),
            ));
        }
        let parts = format::parse(&template.msg)?;
        let fields: Vec<_> = parts
            .iter()
            .filter_map(|part| match part {
                Part::Field(field) => Some(field),
                Part::Literal(_) => None,
            })
            .collect();
        let mut bindings = Vec::new();
        for reference in &template.refs {
            if reference.len() != 1 {
                return Err(FormatError(
                    "each refs entry must contain exactly one name and binding".into(),
                ));
            }
            let (name, binding) = reference.iter().next().expect("one binding");
            let definition =
                relation(name).ok_or_else(|| FormatError(format!("unknown relation {name:?}")))?;
            if definition.kind != RelationKind::Attribute
                || definition.value_type != RelationType::String
            {
                return Err(FormatError(format!("{name:?} is not a string attribute")));
            }
            let Some(field) = fields.get(binding.argument) else {
                return Err(FormatError(
                    "argument binding is outside the statement's placeholders".into(),
                ));
            };
            if field.spec != Spec::String {
                return Err(FormatError(format!(
                    "attribute {name:?} requires a string argument"
                )));
            }
            bindings.push((name.clone(), binding.argument));
        }
        for (index, field) in fields.iter().enumerate() {
            if let Some(name) = field.name.strip_prefix('@')
                && !bindings
                    .iter()
                    .any(|(bound, argument)| bound == name && *argument == index)
            {
                return Err(FormatError(format!(
                    "missing argument binding for {name:?}"
                )));
            }
        }
        Ok(Self {
            template: template.clone(),
            parts,
            bindings,
        })
    }

    fn render(&self, args: &[Argument]) -> Result<String, FormatError> {
        let count = self
            .parts
            .iter()
            .filter(|part| matches!(part, Part::Field(_)))
            .count();
        if count != args.len() {
            return Err(FormatError(format!(
                "format has {count} placeholders but record has {} arguments",
                args.len()
            )));
        }
        let mut arguments = args.iter();
        let mut output = String::new();
        for part in &self.parts {
            match part {
                Part::Literal(text) => output.push_str(text),
                Part::Field(field) => {
                    match (field.spec, arguments.next().expect("validated count")) {
                        (Spec::String, Argument::String(value)) => output.push_str(value),
                        (
                            Spec::Hex {
                                alternate,
                                zero_pad,
                                width,
                            },
                            Argument::Unsigned(value),
                        ) => {
                            let text = match (alternate, zero_pad) {
                                (true, true) => format!("{value:#0width$x}"),
                                (true, false) => format!("{value:#width$x}"),
                                (false, true) => format!("{value:0width$x}"),
                                (false, false) => format!("{value:width$x}"),
                            };
                            output.push_str(&text);
                        }
                        _ => {
                            return Err(FormatError(format!(
                                "argument {} must be {}",
                                field.name,
                                if field.spec == Spec::String {
                                    "a string"
                                } else {
                                    "an unsigned integer"
                                }
                            )));
                        }
                    }
                }
            }
        }
        for (position, (name, index)) in self.bindings.iter().enumerate() {
            let Argument::String(value) = &args[*index] else {
                unreachable!("binding and argument types were validated");
            };
            output.push_str(if position == 0 { "\n" } else { ", " });
            output.extend(name.escape_debug());
            output.push_str(" = ");
            output.extend(value.escape_debug());
        }
        Ok(output)
    }
}

fn statement_id(key: &str) -> Result<u64, FormatError> {
    let suffix = key.strip_prefix(STATEMENT_PREFIX).unwrap_or("");
    let id = suffix.parse::<u64>().ok().filter(|id| *id != 0);
    if suffix.starts_with('0') || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(FormatError(format!("invalid statement identity {key:?}")));
    }
    id.ok_or_else(|| FormatError(format!("invalid statement identity {key:?}")))
}

/// Compiles each statement once and binds its parameters to each event's arguments.
#[derive(Default)]
pub struct Renderer {
    relations: BTreeMap<String, RelationDefinition>,
    statements: BTreeMap<u64, CompiledStatement>,
}

impl Renderer {
    /// Invalid records never change the definition tables.
    pub fn render(&mut self, message: &Message) -> Result<String, FormatError> {
        let id = message
            .attrs
            .get("log.statement")
            .copied()
            .filter(|id| *id != 0 && message.attrs.len() == 1)
            .ok_or_else(|| {
                FormatError(
                    "event attrs must contain only a nonzero log.statement reference".into(),
                )
            })?;
        let mut relations = BTreeMap::new();
        for (key, definition) in &message.defs {
            if let Some(name) = key.strip_prefix(RELATION_PREFIX) {
                let Definition::Relation(definition) = definition else {
                    return Err(FormatError(format!(
                        "expected relation definition for {key:?}"
                    )));
                };
                if name == "log.statement" {
                    if definition.kind != RelationKind::Entity
                        || definition.value_type != RelationType::Object
                        || !definition.anonymous
                    {
                        return Err(FormatError(
                            "log.statement must describe anonymous object entities".into(),
                        ));
                    }
                } else {
                    validate_attribute_name(name)?;
                    if definition.kind != RelationKind::Attribute
                        || definition.value_type != RelationType::String
                        || definition.anonymous
                    {
                        return Err(FormatError(format!("unsupported relation {name:?}")));
                    }
                }
                if let Some(existing) = self.relations.get(name) {
                    if existing != definition {
                        return Err(FormatError(format!("conflicting definition for {name:?}")));
                    }
                } else {
                    relations.insert(name.to_string(), definition.clone());
                }
            } else if !key.starts_with(STATEMENT_PREFIX) {
                return Err(FormatError(format!("unsupported definition {key:?}")));
            }
        }
        let relation = |name: &str| relations.get(name).or_else(|| self.relations.get(name));
        if relation("log.statement").is_none() {
            return Err(FormatError("unknown relation \"log.statement\"".into()));
        }
        let mut statements = BTreeMap::new();
        for (key, definition) in &message.defs {
            if key.starts_with(STATEMENT_PREFIX) {
                let statement_id = statement_id(key)?;
                let Definition::Statement(template) = definition else {
                    return Err(FormatError(format!(
                        "expected statement template for {key:?}"
                    )));
                };
                if let Some(existing) = self.statements.get(&statement_id) {
                    if &existing.template != template {
                        return Err(FormatError(format!("conflicting statement {statement_id}")));
                    }
                } else {
                    statements.insert(
                        statement_id,
                        CompiledStatement::compile(template, relation)?,
                    );
                }
            }
        }
        let statement = statements
            .get(&id)
            .or_else(|| self.statements.get(&id))
            .ok_or_else(|| FormatError(format!("unknown statement {id}")))?;
        let output = statement.render(&message.args)?;
        self.relations.extend(relations);
        self.statements.extend(statements);
        Ok(output)
    }
}
