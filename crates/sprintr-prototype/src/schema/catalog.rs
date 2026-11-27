// SPDX-License-Identifier: Apache-2.0

use crate::{
    Definition, RelationDefinition, RelationType, StatementSource, StatementTemplate,
    declare::Declaration,
    format::{self, Part, Spec},
    runtime::runtime,
    statement::{NestedTemplate, Statement},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io, panic::Location};

#[doc(hidden)]
pub struct LocatedDeclaration {
    pub declaration: Declaration,
    pub module: &'static str,
    pub location: &'static Location<'static>,
}

#[doc(hidden)]
pub enum CatalogTemplate {
    Statement(&'static Statement),
    Structure(&'static NestedTemplate),
}

#[doc(hidden)]
pub struct CatalogSite {
    pub template: CatalogTemplate,
    pub module: &'static str,
    pub location: &'static Location<'static>,
    pub source_format: &'static str,
    pub cause: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CatalogVersion {
    #[serde(rename = "sprintr.catalog/1")]
    V1,
}

/// Static declarations in one linked artifact. Keys use the same recorded
/// names as the writer, including collision renames.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub format: CatalogVersion,
    pub relations: BTreeMap<String, CatalogDeclaration<RelationDefinition>>,
    pub named_values: BTreeMap<String, CatalogDeclaration<sprintr_message::NamedEntity>>,
    pub sites: Vec<Site>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogDeclaration<T> {
    pub definition: T,
    pub origins: Vec<DeclarationOrigin>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclarationOrigin {
    pub declared: String,
    pub source: StatementSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Site {
    pub source: StatementSource,
    pub descriptor: Descriptor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub kind: SiteKind,
    pub source_format: String,
    pub recorded_format: String,
    pub attrs: BTreeMap<String, String>,
    pub arguments: Vec<Slot>,
    pub has_cause: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SiteKind {
    Statement,
    Structure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Slot {
    pub label: String,
    pub attribute: Option<String>,
    pub capture: SlotCapture,
    /// Absent for type-directed `{}` dispatch; the wire type depends on the value.
    pub wire_type: Option<RelationType>,
    pub specifier: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotCapture {
    Attribute,
    String,
    Unsigned,
    DebugText,
    Dynamic,
}

fn source(module: &str, loc: &Location<'_>) -> StatementSource {
    StatementSource {
        module: module.into(),
        file: loc.file().into(),
        line: loc.line(),
        column: loc.column(),
    }
}

/// Enumerate the application's linked schema without starting an output,
/// allocating runtime statement IDs, or evaluating arguments and formatters.
pub fn catalog() -> io::Result<Catalog> {
    let rt = runtime();
    rt.check_schema().map_err(io::Error::other)?;
    let mut relations = BTreeMap::new();
    let mut named_values = BTreeMap::new();
    for d in crate::__private::DECLARATIONS {
        let loc = source(d.module, d.location);
        match &d.declaration {
            Declaration::Relation(meta) => {
                let resolved = &rt.definitions[rt.relation(meta).map_err(io::Error::other)?];
                let Definition::Relation(definition) = &resolved.definition else {
                    unreachable!("relation index")
                };
                relations
                    .entry(resolved.name.clone())
                    .or_insert_with(|| CatalogDeclaration {
                        definition: definition.clone(),
                        origins: Vec::new(),
                    })
                    .origins
                    .push(DeclarationOrigin {
                        declared: meta.name.into(),
                        source: loc,
                    });
            }
            Declaration::Named(value) => {
                let rel = &rt.definitions[rt
                    .relation(value.relation().__meta())
                    .map_err(io::Error::other)?]
                .name;
                let name = &rt.definitions[rt.named(value).map_err(io::Error::other)?].name;
                named_values
                    .entry(format!("{rel}:{name}"))
                    .or_insert_with(|| CatalogDeclaration {
                        definition: sprintr_message::NamedEntity {
                            desc: value.description().into(),
                        },
                        origins: Vec::new(),
                    })
                    .origins
                    .push(DeclarationOrigin {
                        declared: format!("{}:{}", value.relation().name(), value.name()),
                        source: loc,
                    });
            }
        }
    }
    for origins in relations
        .values_mut()
        .map(|d| &mut d.origins)
        .chain(named_values.values_mut().map(|d| &mut d.origins))
    {
        origins.sort();
    }
    let mut sites: Vec<_> = crate::__private::CATALOG_SITES
        .iter()
        .map(|s| s.describe(&relations))
        .collect::<io::Result<_>>()?;
    // Multiple expansions can share an invocation location.
    sites.sort_by_cached_key(|s| {
        (
            s.source.clone(),
            serde_json::to_string(&s.descriptor).expect("catalog descriptor JSON"),
        )
    });
    Ok(Catalog {
        format: CatalogVersion::V1,
        relations,
        named_values,
        sites,
    })
}

impl CatalogSite {
    fn describe(
        &self,
        relations: &BTreeMap<String, CatalogDeclaration<RelationDefinition>>,
    ) -> io::Result<Site> {
        let (kind, source, attrs, msg, bindings) = match self.template {
            CatalogTemplate::Statement(stmt) => {
                let description = stmt.describe().map_err(io::Error::other)?;
                let t: StatementTemplate = serde_json::from_slice(&description.template)?;
                let bindings = t
                    .refs
                    .into_iter()
                    .flatten()
                    .map(|(k, v)| (v.argument, k))
                    .collect::<BTreeMap<_, _>>();
                (SiteKind::Statement, t.source, t.attrs, t.msg, bindings)
            }
            CatalogTemplate::Structure(nested) => {
                let f = nested.resolve()?;
                let bindings = f
                    .bindings
                    .iter()
                    .map(|&(i, r)| (i, runtime().definitions[r].name.clone()))
                    .collect();
                (
                    SiteKind::Structure,
                    source(self.module, self.location),
                    BTreeMap::new(),
                    f.msg.clone(),
                    bindings,
                )
            }
        };
        let fields = |parts: Vec<Part>| {
            parts.into_iter().filter_map(|p| {
                if let Part::Field(f) = p {
                    Some(f)
                } else {
                    None
                }
            })
        };
        let original = fields(format::parse_capture(self.source_format).map_err(io::Error::other)?);
        let mut recorded = fields(format::parse(&msg).map_err(io::Error::other)?);
        let mut arguments = Vec::new();
        for (i, f) in original.enumerate() {
            let label = recorded
                .next()
                .ok_or_else(|| io::Error::other("missing recorded placeholder"))?
                .name;
            let attribute = bindings.get(&i).cloned();
            let (capture, wire_type) = if let Some(attr) = &attribute {
                let relation = relations
                    .get(attr)
                    .ok_or_else(|| io::Error::other(format!("unknown attribute {attr}")))?;
                (
                    SlotCapture::Attribute,
                    Some(relation.definition.value_type.clone()),
                )
            } else {
                match (f.spec, f.format.as_str()) {
                    (Spec::Hex { .. }, _) => (SlotCapture::Unsigned, Some(RelationType::U64)),
                    (_, "s") => (SlotCapture::String, Some(RelationType::String)),
                    (_, "?") => (SlotCapture::DebugText, Some(RelationType::String)),
                    _ => (SlotCapture::Dynamic, None),
                }
            };
            arguments.push(Slot {
                label,
                attribute,
                capture,
                wire_type,
                specifier: f.format,
            });
        }
        if recorded.next().is_some() {
            return Err(io::Error::other("extra recorded placeholder"));
        }
        Ok(Site {
            source,
            descriptor: Descriptor {
                kind,
                source_format: self.source_format.into(),
                recorded_format: msg,
                attrs,
                arguments,
                has_cause: self.cause,
            },
        })
    }
}
