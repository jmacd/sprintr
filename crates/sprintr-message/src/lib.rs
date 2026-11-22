// SPDX-License-Identifier: Apache-2.0

//! Statement templates, event records, and stateful formatted playback.

pub mod format;
mod reference;
#[cfg(test)]
mod tests;

pub use reference::definition_fragment;

use format::{FormatError, Part, Spec};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser::SerializeMap};
use serde_json::{Map, Value};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fmt::Write as _;
use std::time::{Duration, UNIX_EPOCH};

/// The root of self-description. Every other relation, including
/// `log.statement` and `log.level`, is defined by an entity of this relation.
pub const RELATION: &str = "log.relation";
/// The built-in relation whose anonymous entities are statement templates.
///
/// It is declared and defined like any other relation. Readers rely on its
/// name only to find the template that formats an event.
pub const STATEMENT: &str = "log.statement";
/// The anonymous object identifying the thread of an addressed event.
pub const THREAD: &str = "log.thread";
/// The record itself, identified on its thread by the wire field `seq`.
pub const RECORD: &str = "log.record";
pub const SOURCE: &str = "log.source";
pub const STACK: &str = "log.stack";
pub const RELATION_PREFIX: &str = "log.relation:";
pub const STATEMENT_PREFIX: &str = "log.statement:";

/// The value of a named entity, such as `log.level:INFO`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedEntity {
    pub desc: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RelationKind {
    Attribute,
    Entity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RelationType {
    String,
    U64,
    #[serde(rename = "[u64]")]
    U64Array,
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
    /// Attributes describe scalar values or arrays of unsigned integers; entities hold objects.
    pub fn validate(&self, name: &str) -> Result<(), FormatError> {
        validate_relation_name(name)?;
        let supported = match self.kind {
            RelationKind::Attribute => {
                !self.anonymous
                    && matches!(
                        self.value_type,
                        RelationType::String | RelationType::U64 | RelationType::U64Array
                    )
            }
            RelationKind::Entity => self.value_type == RelationType::Object,
        };
        if !supported {
            return Err(FormatError(format!("unsupported relation {name:?}")));
        }
        if matches!(name, STATEMENT | THREAD | RECORD | SOURCE)
            && !(self.kind == RelationKind::Entity && self.anonymous)
        {
            return Err(FormatError(format!(
                "{name} must describe anonymous object entities"
            )));
        }
        if name == STACK && self.value_type != RelationType::U64Array {
            return Err(FormatError(format!(
                "{STACK} must describe a [u64] attribute"
            )));
        }
        Ok(())
    }

    fn describes_named_entities(&self) -> bool {
        self.kind == RelationKind::Entity && !self.anonymous
    }
}

/// Relation names are separated from entity names by `:` and must be
/// representable inside a canonical `{@name}` placeholder. Only the root
/// relation, which is never defined, is unavailable.
pub fn validate_relation_name(name: &str) -> Result<(), FormatError> {
    if name == RELATION {
        return Err(FormatError(format!(
            "{RELATION} is the root relation and cannot be redefined"
        )));
    }
    if name.is_empty() || name.contains(['{', '}', ':']) {
        return Err(FormatError(format!(
            "relation name {name:?} is empty or contains braces/colon"
        )));
    }
    Ok(())
}

/// Named entity names must be nonempty; they follow the first `:` in keys.
pub fn validate_entity_name(name: &str) -> Result<(), FormatError> {
    if name.is_empty() {
        return Err(FormatError("named entity names must not be empty".into()));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Argument {
    String(String),
    Unsigned(u64),
    Reference {
        #[serde(rename = "$ref")]
        target: String,
    },
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
    pub attrs: BTreeMap<String, String>,
    pub refs: Vec<BTreeMap<String, ArgumentBinding>>,
    pub msg: String,
    /// Frame zero, recorded by the compiler at the logging macro invocation.
    pub source: StatementSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatementSource {
    #[serde(rename = "mod")]
    pub module: String,
    pub file: String,
    pub line: u32,
    pub column: u32,
}

/// Start at the nearest `crates/` or `src/`, then keep whole trailing components
/// within 32 characters. Never split the filename itself.
pub fn short_path(path: &str) -> Cow<'_, str> {
    let separators = ['/', '\\'];
    let mut rooted = path;
    let mut offset = 0;
    for part in path.split_inclusive(separators) {
        if matches!(part, "crates/" | "src/" | "crates\\" | "src\\") {
            rooted = &path[offset..];
        }
        offset += part.len();
    }

    let mut tail = rooted;
    while tail.chars().nth(32).is_some() {
        let Some((_, rest)) = tail.split_once(separators) else {
            break;
        };
        if rest.is_empty() {
            break;
        }
        tail = rest;
    }
    if tail.len() == rooted.len() {
        tail.into()
    } else {
        format!(".../{tail}").into()
    }
}

impl fmt::Display for StatementSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "at {} ({}:{})",
            self.module,
            short_path(&self.file),
            self.line
        )
    }
}

/// A definition's key in [`Message::defs`], not the object's shape, determines
/// how it is deserialized.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Definition {
    Relation(RelationDefinition),
    Statement(StatementTemplate),
    Entity(Map<String, Value>),
}

fn deserialize_definitions<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, Definition>, D::Error> {
    struct Definitions;

    impl<'de> de::Visitor<'de> for Definitions {
        type Value = BTreeMap<String, Definition>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an object of keyed definitions")
        }

        fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut defs = BTreeMap::new();
            while let Some(key) = map.next_key::<String>()? {
                let definition = if key.starts_with(RELATION_PREFIX) {
                    Definition::Relation(map.next_value()?)
                } else if key.starts_with(STATEMENT_PREFIX) {
                    Definition::Statement(map.next_value()?)
                } else {
                    Definition::Entity(map.next_value()?)
                };
                match defs.entry(key) {
                    std::collections::btree_map::Entry::Vacant(slot) => {
                        slot.insert(definition);
                    }
                    std::collections::btree_map::Entry::Occupied(slot) => {
                        return Err(de::Error::custom(format!(
                            "duplicate definition {:?}",
                            slot.key()
                        )));
                    }
                }
            }
            Ok(defs)
        }
    }

    deserializer.deserialize_map(Definitions)
}

/// A decoded event, including any definitions introduced in its record.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(try_from = "WireEvent")]
#[must_use]
pub struct Message {
    /// Wall-clock capture time in nanoseconds since the Unix epoch (`ts` on the wire).
    pub timestamp: u64,
    /// Caller PCs starting at frame one; empty when disabled or there are no callers.
    pub stack: Vec<u64>,
    /// Scalar associations, including `log.statement` and, on delivery, `log.thread` and `log.record`.
    pub attrs: BTreeMap<String, u64>,
    pub args: Vec<Argument>,
    pub defs: BTreeMap<String, Definition>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEvent {
    #[serde(default, deserialize_with = "present_u64")]
    seq: Option<u64>,
    #[serde(default, deserialize_with = "present_u64")]
    stmt: Option<u64>,
    #[serde(default, deserialize_with = "present_u64")]
    thread: Option<u64>,
    ts: u64,
    attrs: WireAttrs,
    args: Vec<Argument>,
    #[serde(default, deserialize_with = "deserialize_definitions")]
    defs: BTreeMap<String, Definition>,
}

#[derive(Deserialize)]
struct WireAttrs {
    #[serde(rename = "log.stack")]
    stack: Vec<u64>,
    #[serde(flatten)]
    scalars: BTreeMap<String, u64>,
}

// A field can be absent (longhand or an unaddressed record); null is not a number.
fn present_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    u64::deserialize(d).map(Some)
}

impl TryFrom<WireEvent> for Message {
    type Error = FormatError;

    fn try_from(mut wire: WireEvent) -> Result<Self, FormatError> {
        for (short, name, value) in [
            ("seq", RECORD, wire.seq),
            ("stmt", STATEMENT, wire.stmt),
            ("thread", THREAD, wire.thread),
        ] {
            if let Some(value) = value
                && wire.attrs.scalars.insert(name.into(), value).is_some()
            {
                return Err(FormatError(format!(
                    "both {short} and attrs[{name:?}] specified"
                )));
            }
        }
        Ok(Self {
            timestamp: wire.ts,
            stack: wire.attrs.stack,
            attrs: wire.attrs.scalars,
            args: wire.args,
            defs: wire.defs,
        })
    }
}

impl Serialize for Message {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut wire = serializer.serialize_map(None)?;
        let seq = self.attrs.get(RECORD);
        if let Some(seq) = seq {
            wire.serialize_entry("seq", seq)?;
        }
        let stmt = self
            .attrs
            .get(STATEMENT)
            .ok_or_else(|| serde::ser::Error::custom("missing log.statement"))?;
        wire.serialize_entry("stmt", stmt)?;
        let thread = self.attrs.get(THREAD);
        if let Some(thread) = thread {
            wire.serialize_entry("thread", thread)?;
        }
        wire.serialize_entry("ts", &self.timestamp)?;
        struct EventAttrs<'a>(&'a Message);
        impl Serialize for EventAttrs<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut attrs = serializer.serialize_map(None)?;
                attrs.serialize_entry(STACK, &self.0.stack)?;
                for (key, value) in &self.0.attrs {
                    if key == STACK {
                        return Err(serde::ser::Error::custom("log.stack must be an array"));
                    }
                    if !matches!(key.as_str(), STATEMENT | RECORD | THREAD) {
                        attrs.serialize_entry(key, value)?;
                    }
                }
                attrs.end()
            }
        }
        wire.serialize_entry("attrs", &EventAttrs(self))?;
        wire.serialize_entry("args", &self.args)?;
        if !self.defs.is_empty() {
            wire.serialize_entry("defs", &self.defs)?;
        }
        wire.end()
    }
}

impl Message {
    /// Validate a self-contained record. Stream-dependent events need a [`Renderer`].
    pub fn validate(&self) -> Result<(), FormatError> {
        self.render().map(|_| ())
    }

    pub fn render(&self) -> Result<String, FormatError> {
        Renderer::default().render(self)
    }
}

struct Binding {
    name: String,
    argument: usize,
    value_type: RelationType,
}

struct CompiledStatement {
    template: StatementTemplate,
    parts: Vec<Part>,
    bindings: Vec<Binding>,
    /// Placeholders without a specifier that render unsigned attributes in decimal.
    decimal: Vec<bool>,
}

impl CompiledStatement {
    fn compile<'a>(
        template: &StatementTemplate,
        relation: impl Fn(&str) -> Option<&'a RelationDefinition>,
        has_entity: impl Fn(&str) -> bool,
    ) -> Result<Self, FormatError> {
        for (name, value) in &template.attrs {
            let definition =
                relation(name).ok_or_else(|| FormatError(format!("unknown relation {name:?}")))?;
            if !definition.describes_named_entities() {
                return Err(FormatError(format!(
                    "{name:?} does not describe named entities"
                )));
            }
            if !has_entity(&format!("{name}:{value}")) {
                return Err(FormatError(format!("unknown entity {name}:{value}")));
            }
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
        let mut decimal = vec![false; fields.len()];
        for reference in &template.refs {
            if reference.len() != 1 {
                return Err(FormatError(
                    "each refs entry must contain exactly one name and binding".into(),
                ));
            }
            let (name, binding) = reference.iter().next().expect("one binding");
            let definition =
                relation(name).ok_or_else(|| FormatError(format!("unknown relation {name:?}")))?;
            if definition.kind != RelationKind::Attribute {
                return Err(FormatError(format!("{name:?} is not an attribute")));
            }
            let Some(field) = fields.get(binding.argument) else {
                return Err(FormatError(
                    "argument binding is outside the statement's placeholders".into(),
                ));
            };
            match (definition.value_type, field.spec) {
                (RelationType::String, Spec::String) => {}
                (RelationType::U64, Spec::String) => decimal[binding.argument] = true,
                (RelationType::U64, Spec::Hex { .. }) => {}
                _ => {
                    return Err(FormatError(format!(
                        "placeholder format does not match attribute {name:?}"
                    )));
                }
            }
            bindings.push(Binding {
                name: name.clone(),
                argument: binding.argument,
                value_type: definition.value_type,
            });
        }
        for (index, field) in fields.iter().enumerate() {
            if let Some(name) = field.name.strip_prefix('@')
                && !bindings
                    .iter()
                    .any(|binding| binding.name == name && binding.argument == index)
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
            decimal,
        })
    }

    fn render<'a>(
        &self,
        timestamp: u64,
        args: &[Argument],
        resolve: impl Fn(&str) -> Result<&'a Map<String, Value>, FormatError>,
    ) -> Result<String, FormatError> {
        if self.decimal.len() != args.len() {
            return Err(FormatError(format!(
                "format has {} placeholders but record has {} arguments",
                self.decimal.len(),
                args.len()
            )));
        }
        for binding in &self.bindings {
            if !matches!(
                (binding.value_type, &args[binding.argument]),
                (RelationType::String, Argument::String(_))
                    | (RelationType::U64, Argument::Unsigned(_))
            ) {
                return Err(FormatError(format!(
                    "argument for {:?} has the wrong type",
                    binding.name
                )));
            }
        }
        let mut arguments = args.iter().enumerate();
        let instant = UNIX_EPOCH
            .checked_add(Duration::from_nanos(timestamp))
            .ok_or_else(|| {
                FormatError(format!(
                    "timestamp {timestamp} is outside the platform time range"
                ))
            })?;
        let mut output = format!("{} ", humantime::format_rfc3339_millis(instant));
        for part in &self.parts {
            match part {
                Part::Literal(text) => output.push_str(text),
                Part::Field(field) => {
                    let (index, argument) = arguments.next().expect("validated count");
                    match (field.spec, argument) {
                        (Spec::String, Argument::String(value)) => output.push_str(value),
                        (Spec::String, Argument::Reference { target }) => {
                            let object = resolve(target)?;
                            output.push_str(&serde_json::to_string(object).map_err(|e| {
                                FormatError(format!("cannot render entity {target:?}: {e}"))
                            })?);
                        }
                        (Spec::String, Argument::Unsigned(value)) if self.decimal[index] => {
                            write!(output, "{value}").expect("string formatting");
                        }
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
        for (position, (name, value)) in self.template.attrs.iter().enumerate() {
            output.push_str(if position == 0 { "\n" } else { ", " });
            output.extend(name.escape_debug());
            output.push_str(" = ");
            output.extend(value.escape_debug());
        }
        for (position, binding) in self.bindings.iter().enumerate() {
            output.push_str(if position == 0 && self.template.attrs.is_empty() {
                "\n"
            } else {
                ", "
            });
            output.extend(binding.name.escape_debug());
            output.push_str(" = ");
            match &args[binding.argument] {
                Argument::String(value) => output.extend(value.escape_debug()),
                Argument::Unsigned(value) => write!(output, "{value}").expect("string formatting"),
                Argument::Reference { .. } => unreachable!("validated primitive binding"),
            }
        }
        Ok(output)
    }
}

fn statement_id(key: &str) -> Result<u64, FormatError> {
    let suffix = key.strip_prefix(STATEMENT_PREFIX).unwrap_or("");
    reference::positive_id(suffix)
        .ok_or_else(|| FormatError(format!("invalid statement identity {key:?}")))
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct RecordAddress {
    thread: u64,
    sequence: u64,
}

struct StoredEntity {
    object: Map<String, Value>,
    /// Only records whose `defs` actually contained this object.
    definitions: BTreeSet<RecordAddress>,
}

fn event_identity(
    attrs: &BTreeMap<String, u64>,
) -> Result<(u64, Option<RecordAddress>), FormatError> {
    let statement = attrs
        .get(STATEMENT)
        .copied()
        .filter(|id| *id != 0)
        .ok_or_else(|| FormatError(format!("expected a nonzero {STATEMENT} reference")))?;
    let address = match (attrs.get(THREAD), attrs.get(RECORD)) {
        (None, None) if attrs.len() == 1 => None,
        (Some(&thread), Some(&sequence)) if attrs.len() == 3 && thread != 0 => {
            Some(RecordAddress { thread, sequence })
        }
        _ => {
            return Err(FormatError(format!(
                "event must contain only stmt ({STATEMENT}), or nonzero stmt and thread ({THREAD}) with seq ({RECORD})"
            )));
        }
    };
    Ok((statement, address))
}

/// Compiles each statement once and binds its parameters to each event's arguments.
#[derive(Default)]
pub struct Renderer {
    relations: BTreeMap<String, RelationDefinition>,
    entities: BTreeMap<String, StoredEntity>,
    statements: BTreeMap<u64, CompiledStatement>,
    last_record: BTreeMap<u64, u64>,
}

impl Renderer {
    /// Static frame zero for a statement already accepted by this reader.
    pub fn statement_source(&self, id: u64) -> Option<&StatementSource> {
        self.statements.get(&id).map(|s| &s.template.source)
    }

    /// Invalid records never install definitions or advance a thread's sequence.
    pub fn render(&mut self, message: &Message) -> Result<String, FormatError> {
        let (id, address) = event_identity(&message.attrs)?;
        if let Some(address) = address
            && self
                .last_record
                .get(&address.thread)
                .is_some_and(|&last| address.sequence <= last)
        {
            return Err(FormatError(format!(
                "{RECORD} {} does not increase on {THREAD} {}",
                address.sequence, address.thread
            )));
        }
        let mut relations = BTreeMap::new();
        for (key, definition) in &message.defs {
            let Some(name) = key.strip_prefix(RELATION_PREFIX) else {
                continue;
            };
            let Definition::Relation(definition) = definition else {
                return Err(FormatError(format!(
                    "expected relation definition for {key:?}"
                )));
            };
            definition.validate(name)?;
            if let Some(existing) = self.relations.get(name) {
                if existing != definition {
                    return Err(FormatError(format!("conflicting definition for {name:?}")));
                }
            } else {
                relations.insert(name.to_string(), definition.clone());
            }
        }
        let relation = |name: &str| relations.get(name).or_else(|| self.relations.get(name));
        for name in [STATEMENT, STACK, SOURCE] {
            if relation(name).is_none() {
                return Err(FormatError(format!("unknown relation {name:?}")));
            }
        }
        if message.stack.contains(&0) {
            return Err(FormatError(format!(
                "{STACK} requires nonzero program counters"
            )));
        }
        for (key, definition) in &message.defs {
            if key.starts_with(RELATION_PREFIX) || key.starts_with(STATEMENT_PREFIX) {
                continue;
            }
            let (relation_name, name) = key
                .split_once(':')
                .ok_or_else(|| FormatError(format!("unsupported definition {key:?}")))?;
            let described = relation(relation_name)
                .ok_or_else(|| FormatError(format!("unknown relation {relation_name:?}")))?;
            let Definition::Entity(object) = definition else {
                return Err(FormatError(format!("expected entity object for {key:?}")));
            };
            if described.kind != RelationKind::Entity {
                return Err(FormatError(format!(
                    "{relation_name:?} is not an entity relation"
                )));
            }
            if relation_name == RECORD {
                return Err(FormatError(format!(
                    "{RECORD} is defined by the record itself"
                )));
            }
            if described.anonymous {
                if reference::positive_id(name).is_none() {
                    return Err(FormatError(format!(
                        "invalid anonymous entity identity {key:?}"
                    )));
                }
            } else {
                validate_entity_name(name)?;
                if object.len() != 1 || !object.get("desc").is_some_and(Value::is_string) {
                    return Err(FormatError(format!(
                        "named entity {key:?} must contain only a string desc"
                    )));
                }
            }
            if self
                .entities
                .get(key)
                .is_some_and(|existing| &existing.object != object)
            {
                return Err(FormatError(format!("conflicting definition for {key:?}")));
            }
        }
        let has_entity = |key: &str| {
            matches!(message.defs.get(key), Some(Definition::Entity(_)))
                || self.entities.contains_key(key)
        };
        if let Some(address) = address {
            for name in [THREAD, RECORD] {
                if relation(name).is_none() {
                    return Err(FormatError(format!("unknown relation {name:?}")));
                }
            }
            let thread = format!("{THREAD}:{}", address.thread);
            if !has_entity(&thread) {
                return Err(FormatError(format!("unknown thread entity {thread:?}")));
            }
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
                        CompiledStatement::compile(template, relation, has_entity)?,
                    );
                }
            }
        }
        let statement = statements
            .get(&id)
            .or_else(|| self.statements.get(&id))
            .ok_or_else(|| FormatError(format!("unknown statement {id}")))?;
        let output = statement.render(message.timestamp, &message.args, |target| {
            self.resolve(target, address, message)
        })?;
        self.relations.extend(relations);
        self.statements.extend(statements);
        for (key, definition) in &message.defs {
            if let Definition::Entity(object) = definition {
                let entity = self
                    .entities
                    .entry(key.clone())
                    .or_insert_with(|| StoredEntity {
                        object: object.clone(),
                        definitions: BTreeSet::new(),
                    });
                if let Some(address) = address {
                    entity.definitions.insert(address);
                }
            }
        }
        if let Some(address) = address {
            self.last_record.insert(address.thread, address.sequence);
        }
        Ok(output)
    }

    fn resolve<'a>(
        &'a self,
        target: &str,
        address: Option<RecordAddress>,
        message: &'a Message,
    ) -> Result<&'a Map<String, Value>, FormatError> {
        let (sequence, key) = reference::parse(target)?;
        if let Some(sequence) = sequence {
            let current = address.ok_or_else(|| {
                FormatError(format!("reference {target:?} requires an addressed record"))
            })?;
            if sequence != current.sequence {
                let origin = RecordAddress {
                    sequence,
                    ..current
                };
                return self
                    .entities
                    .get(&key)
                    .filter(|entity| entity.definitions.contains(&origin))
                    .map(|entity| &entity.object)
                    .ok_or_else(|| {
                        FormatError(format!(
                            "no entity definition at {target:?} on {THREAD} {}",
                            current.thread
                        ))
                    });
            }
        }
        match message.defs.get(&key) {
            Some(Definition::Entity(object)) => Ok(object),
            _ => Err(FormatError(format!(
                "no entity definition at {target:?} in this record"
            ))),
        }
    }
}
