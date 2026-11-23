// SPDX-License-Identifier: Apache-2.0

//! Statement templates, event records, and stateful formatted playback.

mod argument;
pub mod format;
mod reference;
mod structured;
#[cfg(test)]
mod tests;

pub use argument::Argument;
pub use reference::{definition_fragment, push_pointer_token};

use format::{FormatError, Part, Spec};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser::SerializeMap};
use serde_json::{Map, Value};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::{Duration, UNIX_EPOCH};
use structured::{CompiledEncoder, RecordArgs};

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
/// A record `refs` key with this relation names a literal argument object.
pub const STRUCT_NAME: &str = "struct.name";
/// A named definition describing how to render a positional structured argument.
pub const STRUCT_ENCODE: &str = "struct.encode";
pub const STRUCT_ENCODE_PREFIX: &str = "struct.encode:";
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RelationType {
    String,
    U64,
    #[serde(rename = "[u64]")]
    U64Array,
    Object,
    /// An argument referring to an entity of this recorded relation.
    Entity(String),
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
    /// Attributes label values; an entity-valued attribute declares its target relation.
    pub fn validate(&self, name: &str) -> Result<(), FormatError> {
        validate_relation_name(name)?;
        let supported = match self.kind {
            RelationKind::Attribute => {
                !self.anonymous
                    && matches!(
                        self.value_type,
                        RelationType::String
                            | RelationType::U64
                            | RelationType::U64Array
                            | RelationType::Entity(_)
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
        if matches!(name, STRUCT_NAME | STRUCT_ENCODE) && !self.describes_named_entities() {
            return Err(FormatError(format!(
                "{name} must describe named object entities"
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

/// Immutable formatter metadata at a `structured!` invocation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncodeDefinition {
    pub msg: String,
    pub name: String,
    pub file: String,
    pub line: u32,
}

/// A definition's key in [`Message::defs`], not the object's shape, determines
/// how it is deserialized.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Definition {
    Relation(RelationDefinition),
    Statement(StatementTemplate),
    Encoder(EncodeDefinition),
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
                } else if key.starts_with(STRUCT_ENCODE_PREFIX) {
                    Definition::Encoder(map.next_value()?)
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
    /// Entity-valued associations from event `attrs`, separate from positional arguments.
    pub context: BTreeMap<String, ContextRef>,
    pub args: Vec<Argument>,
    /// Ordered, single-entry associations with local JSON Pointer fragments into `args`.
    pub refs: Vec<BTreeMap<String, String>>,
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
    #[serde(default)]
    refs: Vec<WireRef>,
    #[serde(default, deserialize_with = "deserialize_definitions")]
    defs: BTreeMap<String, Definition>,
}

/// A process-local entity key in event context, not a record-relative JSON Pointer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextRef {
    #[serde(rename = "$ref")]
    pub entity: String,
}

struct WireAttrs {
    stack: Vec<u64>,
    scalars: BTreeMap<String, u64>,
    context: BTreeMap<String, ContextRef>,
}

impl<'de> Deserialize<'de> for WireAttrs {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = WireAttrs;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("event attributes with a stack and optional entity associations")
            }
            fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<WireAttrs, A::Error> {
                let mut stack = None;
                let mut scalars = BTreeMap::new();
                let mut context = BTreeMap::new();
                while let Some(key) = map.next_key::<String>()? {
                    if match key.as_str() {
                        STACK => stack.replace(map.next_value()?).is_some(),
                        THREAD | STATEMENT | RECORD => {
                            scalars.insert(key, map.next_value()?).is_some()
                        }
                        _ => context.insert(key, map.next_value()?).is_some(),
                    } {
                        return Err(de::Error::custom("duplicate event attribute"));
                    }
                }
                Ok(WireAttrs {
                    stack: stack.ok_or_else(|| de::Error::missing_field(STACK))?,
                    scalars,
                    context,
                })
            }
        }
        d.deserialize_map(Visitor)
    }
}

// Unlike deserializing straight into a map, this also rejects two entries
// with the same JSON key (which would otherwise collapse to a single entry).
struct WireRef(String, String);

impl<'de> Deserialize<'de> for WireRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = WireRef;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a single-entry name to fragment map")
            }

            fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<WireRef, A::Error> {
                let Some((name, fragment)) = map.next_entry()? else {
                    return Err(de::Error::invalid_length(0, &self));
                };
                if map.next_key::<de::IgnoredAny>()?.is_some() {
                    return Err(de::Error::invalid_length(2, &self));
                }
                Ok(WireRef(name, fragment))
            }
        }
        d.deserialize_map(Visitor)
    }
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
            context: wire.attrs.context,
            args: wire.args,
            refs: wire
                .refs
                .into_iter()
                .map(|r| BTreeMap::from([(r.0, r.1)]))
                .collect(),
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
                for (key, value) in &self.0.context {
                    if key == STACK
                        || self.0.attrs.contains_key(key)
                        || matches!(key.as_str(), THREAD | STATEMENT | RECORD)
                    {
                        return Err(serde::ser::Error::custom(format!(
                            "conflicting context attribute {key:?}"
                        )));
                    }
                    attrs.serialize_entry(key, value)?;
                }
                attrs.end()
            }
        }
        wire.serialize_entry("attrs", &EventAttrs(self))?;
        wire.serialize_entry("args", &self.args)?;
        if !self.refs.is_empty() {
            if self.refs.iter().any(|entry| entry.len() != 1) {
                return Err(serde::ser::Error::custom(
                    "each event refs entry must have exactly one name and path",
                ));
            }
            wire.serialize_entry("refs", &self.refs)?;
        }
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
            match (&definition.value_type, field.spec) {
                (RelationType::String | RelationType::Entity(_), Spec::String) => {}
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
                value_type: definition.value_type.clone(),
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

    fn render(&self, message: &Message, record: &RecordArgs<'_>) -> Result<String, FormatError> {
        let args = record.args();
        if self.decimal.len() != args.len() {
            return Err(FormatError(format!(
                "format has {} placeholders but record has {} arguments",
                self.decimal.len(),
                args.len()
            )));
        }
        for binding in &self.bindings {
            args[binding.argument].check_attribute(&binding.name, &binding.value_type)?;
        }
        let mut index = 0;
        let timestamp = message.timestamp;
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
                    record.write_root(&mut output, index, field, self.decimal[index])?;
                    index += 1;
                }
            }
        }
        let attrs = self
            .template
            .attrs
            .iter()
            .chain(message.context.iter().map(|(k, v)| (k, &v.entity)));
        for (position, (name, value)) in attrs.enumerate() {
            output.push_str(if position == 0 { "\n" } else { ", " });
            output.extend(name.escape_debug());
            output.push_str(" = ");
            output.extend(value.escape_debug());
        }
        let bindings = self
            .bindings
            .iter()
            .map(|b| (b.name.as_str(), &args[b.argument]));
        for (position, (name, value)) in bindings
            .chain(record.associations.iter().copied())
            .enumerate()
        {
            output.push_str(
                if position == 0 && self.template.attrs.is_empty() && message.context.is_empty() {
                    "\n"
                } else {
                    ", "
                },
            );
            output.extend(name.escape_debug());
            output.push_str(" = ");
            record.write_association(&mut output, value)?;
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
    encoders: BTreeMap<String, CompiledEncoder>,
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
        for definition in relations.values() {
            if let RelationType::Entity(target) = &definition.value_type
                && !relation(target).is_some_and(|r| r.kind == RelationKind::Entity)
            {
                return Err(FormatError(format!(
                    "expected an entity relation for attribute target {target:?}"
                )));
            }
        }
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
        let mut encoders = BTreeMap::new();
        for (key, definition) in &message.defs {
            if key.starts_with(RELATION_PREFIX) || key.starts_with(STATEMENT_PREFIX) {
                continue;
            }
            let (relation_name, name) = key
                .split_once(':')
                .ok_or_else(|| FormatError(format!("unsupported definition {key:?}")))?;
            let described = relation(relation_name)
                .ok_or_else(|| FormatError(format!("unknown relation {relation_name:?}")))?;
            if relation_name == STRUCT_ENCODE {
                validate_entity_name(name)?;
                let Definition::Encoder(definition) = definition else {
                    return Err(FormatError(format!(
                        "expected encoder definition for {key:?}"
                    )));
                };
                if let Some(existing) = self.encoders.get(key) {
                    if &existing.definition != definition {
                        return Err(FormatError(format!("conflicting definition for {key:?}")));
                    }
                } else {
                    encoders.insert(key.clone(), CompiledEncoder::compile(definition)?);
                }
                continue;
            }
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
            matches!(
                message.defs.get(key),
                Some(Definition::Entity(_) | Definition::Encoder(_))
            ) || self.entities.contains_key(key)
                || self.encoders.contains_key(key)
        };
        for (role, value) in &message.context {
            let Some(RelationDefinition {
                kind: RelationKind::Attribute,
                value_type: RelationType::Entity(expected),
                ..
            }) = relation(role)
            else {
                return Err(FormatError(format!(
                    "expected an entity-valued context attribute {role:?}"
                )));
            };
            if value
                .entity
                .split_once(':')
                .is_none_or(|(target, _)| target != expected)
                || !has_entity(&value.entity)
            {
                return Err(FormatError(format!(
                    "invalid or unknown context entity {:?} for {role:?} (expected {expected:?})",
                    value.entity
                )));
            }
        }
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
        let arguments = RecordArgs::new(self, message, address, relation, |key| {
            encoders.get(key).or_else(|| self.encoders.get(key))
        })?;
        let output = statement.render(message, &arguments)?;
        self.encoders.extend(encoders);
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

    fn resolve_definition<'a>(
        &'a self,
        sequence: Option<u64>,
        key: &str,
        target: &str,
        address: Option<RecordAddress>,
        message: &'a Message,
    ) -> Result<&'a Map<String, Value>, FormatError> {
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
                    .get(key)
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
        match message.defs.get(key) {
            Some(Definition::Entity(object)) => Ok(object),
            _ => Err(FormatError(format!(
                "no entity definition at {target:?} in this record"
            ))),
        }
    }
}
