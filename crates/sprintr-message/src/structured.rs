// SPDX-License-Identifier: Apache-2.0

use super::{
    Argument, EncodeDefinition, Message, RecordAddress, RelationDefinition, RelationKind, Renderer,
    STRUCT_ENCODE, STRUCT_NAME, SpanLinks, format, reference, span::SpanRefs, validate_entity_name,
};
use format::{Field, FormatError, Part, Spec};
use reference::Target;
use std::{collections::BTreeMap, fmt::Write as _};

#[derive(Clone, Copy)]
enum Layout<'a> {
    Named(&'a str),
    Formatted(&'a [Part]),
}

#[derive(Default)]
struct Annotations<'a> {
    layout: Option<Layout<'a>>,
    attributes: Vec<&'a str>,
}

/// Ephemeral for one render: lookup uses decoded paths (not URI spellings),
/// while association display follows wire order, including repeated names.
pub(super) struct RecordArgs<'a> {
    renderer: &'a Renderer,
    message: &'a Message,
    address: Option<RecordAddress>,
    annotations: BTreeMap<Vec<String>, Annotations<'a>>,
    pub associations: Vec<(&'a str, &'a Argument)>,
    pub span_links: Option<SpanLinks>,
    pub inline_starts: Vec<RecordAddress>,
    pub inline_exceptions: Vec<super::ExceptionTarget>,
}

impl<'a> RecordArgs<'a> {
    pub fn new<'r>(
        renderer: &'a Renderer,
        message: &'a Message,
        address: Option<RecordAddress>,
        relation: impl Fn(&str) -> Option<&'r RelationDefinition>,
        encoder: impl Fn(&str) -> Option<&'a CompiledEncoder>,
    ) -> Result<Self, FormatError> {
        let mut record = Self {
            renderer,
            message,
            address,
            annotations: BTreeMap::new(),
            associations: Vec::new(),
            span_links: None,
            inline_starts: Vec::new(),
            inline_exceptions: Vec::new(),
        };
        let mut spans = SpanRefs::default();
        for entry in message.event_refs() {
            let (key, fragment) = entry?;
            let (name, suffix) = key
                .split_once(':')
                .map_or((key, None), |(name, value)| (name, Some(value)));
            let def =
                relation(name).ok_or_else(|| FormatError(format!("unknown relation {name:?}")))?;
            if spans.collect(key, fragment)? {
                continue;
            }
            let Target::Argument(path) = reference::parse(fragment)? else {
                return Err(FormatError(format!(
                    "expected a local argument path, got {fragment:?}"
                )));
            };
            let arg = Argument::at(&message.args, &path)
                .ok_or_else(|| FormatError(format!("no argument at {fragment:?}")))?;
            match (name, suffix) {
                // Relation validation reserves these names for named object relations.
                (STRUCT_NAME | STRUCT_ENCODE, Some(value)) => {
                    let layout = match (name, arg) {
                        (STRUCT_NAME, Argument::Object(_)) => {
                            validate_entity_name(value)?;
                            Layout::Named(value)
                        }
                        (STRUCT_ENCODE, Argument::Array(array)) => {
                            let compiled = encoder(key)
                                .ok_or_else(|| FormatError(format!("unknown encoder {key:?}")))?;
                            if compiled.arity != array.len() {
                                return Err(FormatError(format!(
                                    "encoder {key:?} does not match array of length {}",
                                    array.len()
                                )));
                            }
                            Layout::Formatted(&compiled.parts)
                        }
                        _ => {
                            return Err(FormatError(format!(
                                "wrong argument type for {name} at {fragment:?}"
                            )));
                        }
                    };
                    if record
                        .annotations
                        .entry(path)
                        .or_default()
                        .layout
                        .replace(layout)
                        .is_some()
                    {
                        return Err(FormatError(format!("duplicate layout at {fragment:?}")));
                    }
                }
                (_, None) if def.kind == RelationKind::Attribute => {
                    arg.check_attribute(name, &def.value_type)?;
                    record
                        .annotations
                        .entry(path)
                        .or_default()
                        .attributes
                        .push(name);
                    record.associations.push((name, arg));
                }
                (_, Some(id)) if def.anonymous && reference::positive_id(id).is_some() => {
                    if matches!(arg, Argument::Object(_)) {
                        // Names this literal occurrence, not a second entry in defs.
                        continue;
                    }
                    // Older writers also named reference slots. Accept that only
                    // when the annotation agrees with the identity in the $ref.
                    if let Argument::Reference { target } = arg
                        && let Target::Definition {
                            key: referenced, ..
                        } = reference::parse(target)?
                        && referenced == key
                    {
                        continue;
                    }
                    return Err(FormatError(format!(
                        "entity {key:?} does not match the reference at {fragment:?}"
                    )));
                }
                _ => return Err(FormatError(format!("invalid event association {key:?}"))),
            }
        }
        record.span_links = spans.finish()?;
        // Check after collecting layouts: the refs list need not be in traversal order.
        for (path, node) in &record.annotations {
            if !node.attributes.is_empty()
                && path.len() > 1
                && !matches!(
                    record.layout(&path[..path.len() - 1]),
                    Some(Layout::Formatted(_))
                )
            {
                return Err(FormatError(
                    "attributes bind positional arguments of a statement or structured formatter"
                        .into(),
                ));
            }
        }
        Ok(record)
    }

    pub fn args(&self) -> &'a [Argument] {
        &self.message.args
    }

    pub fn write_association(
        &mut self,
        out: &mut String,
        arg: &Argument,
    ) -> Result<(), FormatError> {
        if let Argument::String(s) = arg {
            out.extend(s.escape_debug());
            return Ok(());
        }
        self.write_value(out, &mut Vec::new(), arg, false)
    }

    pub fn write_root(
        &mut self,
        out: &mut String,
        index: usize,
        field: &Field,
        decimal: bool,
    ) -> Result<(), FormatError> {
        let arg = &self.args()[index];
        // Old scalar and entity records need no argument path.
        let mut path = match arg {
            Argument::Object(_) | Argument::Array(_) => vec![index.to_string()],
            _ => Vec::new(),
        };
        self.write_field(out, &mut path, field, arg, decimal)
    }

    fn write_field(
        &mut self,
        out: &mut String,
        path: &mut Vec<String>,
        field: &Field,
        arg: &Argument,
        decimal: bool,
    ) -> Result<(), FormatError> {
        match (field.spec, arg) {
            (Spec::String, Argument::Unsigned(_)) if !decimal => Err(FormatError(format!(
                "argument {} must be a string",
                field.name
            ))),
            (Spec::String, _) => self.write_value(out, path, arg, false),
            (
                Spec::Hex {
                    alternate,
                    zero_pad,
                    width,
                },
                Argument::Unsigned(value),
            ) => {
                match (alternate, zero_pad) {
                    (true, true) => write!(out, "{value:#0width$x}"),
                    (true, false) => write!(out, "{value:#width$x}"),
                    (false, true) => write!(out, "{value:0width$x}"),
                    (false, false) => write!(out, "{value:width$x}"),
                }
                .expect("string formatting");
                Ok(())
            }
            _ => Err(FormatError(format!(
                "argument {} must be an unsigned integer",
                field.name
            ))),
        }
    }

    fn write_value(
        &mut self,
        out: &mut String,
        path: &mut Vec<String>,
        arg: &Argument,
        debug: bool,
    ) -> Result<(), FormatError> {
        match arg {
            Argument::String(s) if debug => write!(out, "{s:?}").expect("string formatting"),
            Argument::String(s) => out.push_str(s),
            Argument::Unsigned(n) => write!(out, "{n}").expect("string formatting"),
            Argument::Reference { target } => match reference::parse(target)? {
                Target::Exception(link) => {
                    self.inline_exceptions.push(link);
                    write!(out, "{}", link.diagnostic()).expect("write to string");
                }
                Target::SpanStart(address) => {
                    self.inline_starts.push(address);
                    out.push_str(target); // Diagnostic address, not a recursive expansion.
                }
                Target::Definition { sequence, key } => {
                    let object = self.renderer.resolve_definition(
                        sequence,
                        &key,
                        target,
                        self.address,
                        self.message,
                    )?;
                    out.push_str(
                        &serde_json::to_string(object).map_err(|e| FormatError(e.to_string()))?,
                    );
                }
                Target::Argument(ancestor) => {
                    // A strict prefix is an existing container on this descent.
                    // Never follow the edge (or allow a dangling/self/sibling ref).
                    if ancestor.len() >= path.len() || !path.starts_with(&ancestor) {
                        return Err(FormatError(format!(
                            "inline reference {target:?} does not name an ancestor"
                        )));
                    }
                    out.push_str(
                        &serde_json::to_string(arg).map_err(|e| FormatError(e.to_string()))?,
                    );
                }
            },
            Argument::Object(object) => {
                if let Some(Layout::Named(name)) = self.layout(path) {
                    out.extend(name.escape_debug());
                    out.push(' ');
                }
                out.push('{');
                for (i, (key, child)) in object.iter().enumerate() {
                    if key == "$ref" {
                        return Err(FormatError("$ref is reserved for inline references".into()));
                    }
                    out.push_str(if i == 0 { " " } else { ", " });
                    out.extend(key.escape_debug());
                    out.push_str(": ");
                    path.push(key.clone());
                    self.write_value(out, path, child, true)?;
                    path.pop();
                }
                if !object.is_empty() {
                    out.push(' ');
                }
                out.push('}');
            }
            Argument::Array(array) => {
                if let Some(Layout::Formatted(parts)) = self.layout(path) {
                    let mut children = array.iter().enumerate();
                    for part in parts {
                        match part {
                            Part::Literal(text) => out.push_str(text),
                            Part::Field(field) => {
                                let (i, child) = children.next().expect("validated layout arity");
                                path.push(i.to_string());
                                if let Some(name) = field.name.strip_prefix('@')
                                    && !self
                                        .annotations
                                        .get(path.as_slice())
                                        .is_some_and(|node| node.attributes.contains(&name))
                                {
                                    return Err(FormatError(format!(
                                        "missing argument binding for {name:?}"
                                    )));
                                }
                                self.write_field(out, path, field, child, true)?;
                                path.pop();
                            }
                        }
                    }
                } else {
                    out.push('[');
                    for (i, child) in array.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        path.push(i.to_string());
                        self.write_value(out, path, child, true)?;
                        path.pop();
                    }
                    out.push(']');
                }
            }
        }
        Ok(())
    }

    fn layout(&self, path: &[String]) -> Option<Layout<'a>> {
        self.annotations.get(path)?.layout
    }
}

pub(super) struct CompiledEncoder {
    pub definition: EncodeDefinition,
    parts: Vec<Part>,
    arity: usize,
}

impl CompiledEncoder {
    pub fn compile(definition: &EncodeDefinition) -> Result<Self, FormatError> {
        if definition.name.is_empty() || definition.file.is_empty() || definition.line == 0 {
            return Err(FormatError(
                "encoder requires a type name, file, and one-based source line".into(),
            ));
        }
        let template = &definition.msg;
        let parts = format::parse(template)?;
        if parts
            .iter()
            .any(|p| matches!(p, Part::Field(f) if f.format == "s"))
            || format::unparse(&parts) != *template
        {
            return Err(FormatError(format!(
                "expected a canonical stored format, got {template:?}"
            )));
        }
        let arity = parts.iter().filter(|p| matches!(p, Part::Field(_))).count();
        Ok(Self {
            definition: definition.clone(),
            parts,
            arity,
        })
    }
}
