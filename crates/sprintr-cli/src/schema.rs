// SPDX-License-Identifier: Apache-2.0

use clap::{Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::{Value, json};
use sprintr_prototype::{
    StatementSource,
    schema::{Catalog, CatalogDeclaration, Site, SiteKind},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Subcommand)]
pub(super) enum Command {
    /// Report schema changes. Differences are informational and return success.
    Diff {
        base: PathBuf,
        head: PathBuf,
        #[arg(long, value_enum, default_value_t = Format::Markdown)]
        format: Format,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub(super) enum Format {
    Markdown,
    Json,
}

pub(super) fn run(Command::Diff { base, head, format }: Command) -> io::Result<()> {
    let read = |path: &Path| {
        serde_json::from_reader(super::open(path)?).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {e}", path.display()),
            )
        })
    };
    let changes = compare(&read(&base)?, &read(&head)?);
    let mut out = io::stdout().lock();
    match format {
        Format::Json => {
            serde_json::to_writer_pretty(
                &mut out,
                &json!({"format":"sprintr.diff/1","changes":changes}),
            )?;
            writeln!(out)?;
        }
        Format::Markdown => markdown(&mut out, &changes)?,
    }
    out.flush()
}

#[derive(Serialize)]
struct Change {
    change: &'static str,
    item: &'static str,
    name: String,
    before_sources: Vec<StatementSource>,
    after_sources: Vec<StatementSource>,
    fields: Vec<FieldChange>,
}

#[derive(Serialize)]
struct FieldChange {
    /// JSON Pointer relative to the item's descriptor. Empty means the entire item.
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    before: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    after: Option<Value>,
}

fn fields(path: &str, old: Option<&Value>, new: Option<&Value>, out: &mut Vec<FieldChange>) {
    if old == new {
        return;
    }
    match (old, new) {
        (Some(Value::Object(a)), Some(Value::Object(b))) => {
            for key in a.keys().chain(b.keys()).collect::<BTreeSet<_>>() {
                let token = key.replace('~', "~0").replace('/', "~1");
                fields(&format!("{path}/{token}"), a.get(key), b.get(key), out);
            }
        }
        (Some(Value::Array(a)), Some(Value::Array(b))) => {
            for i in 0..a.len().max(b.len()) {
                fields(&format!("{path}/{i}"), a.get(i), b.get(i), out);
            }
        }
        _ => out.push(FieldChange {
            path: path.into(),
            before: old.cloned(),
            after: new.cloned(),
        }),
    }
}

type Facts = (Value, Vec<StatementSource>);

fn change(
    item: &'static str,
    name: &str,
    before: Option<Facts>,
    after: Option<Facts>,
) -> Option<Change> {
    let kind = match (&before, &after) {
        (None, Some(_)) => "added",
        (Some(_), None) => "removed",
        _ => "modified",
    };
    let mut delta = Vec::new();
    fields(
        "",
        before.as_ref().map(|f| &f.0),
        after.as_ref().map(|f| &f.0),
        &mut delta,
    );
    (!delta.is_empty()).then(|| Change {
        change: kind,
        item,
        name: name.into(),
        before_sources: before.map(|f| f.1).unwrap_or_default(),
        after_sources: after.map(|f| f.1).unwrap_or_default(),
        fields: delta,
    })
}

fn declarations<T: Serialize>(
    item: &'static str,
    a: &BTreeMap<String, CatalogDeclaration<T>>,
    b: &BTreeMap<String, CatalogDeclaration<T>>,
    out: &mut Vec<Change>,
) {
    let facts = |d: &CatalogDeclaration<T>| {
        let mut names = BTreeMap::new();
        for origin in &d.origins {
            *names.entry(&origin.declared).or_insert(0usize) += 1;
        }
        (
            json!({"declared":names,"definition":d.definition}),
            d.origins.iter().map(|o| o.source.clone()).collect(),
        )
    };
    for k in a.keys().chain(b.keys()).collect::<BTreeSet<_>>() {
        out.extend(change(item, k, a.get(k).map(facts), b.get(k).map(facts)));
    }
}

fn groups(
    sites: &[Option<&Site>],
    key: &impl Fn(&Site) -> Option<String>,
) -> BTreeMap<String, Vec<usize>> {
    let mut groups = BTreeMap::<_, Vec<_>>::new();
    for (i, s) in sites.iter().enumerate() {
        if let Some(k) = s.and_then(key) {
            groups.entry(k).or_default().push(i);
        }
    }
    groups
}

fn site_change(before: Option<&Site>, after: Option<&Site>) -> Option<Change> {
    let s = after.or(before)?;
    let item = match s.descriptor.kind {
        SiteKind::Statement => "statement",
        SiteKind::Structure => "structure",
    };
    let facts = |s: &Site| (json!(s.descriptor), vec![s.source.clone()]);
    change(
        item,
        &s.descriptor.recorded_format,
        before.map(facts),
        after.map(facts),
    )
}

fn compare(a: &Catalog, b: &Catalog) -> Vec<Change> {
    let mut out = Vec::new();
    declarations("relation", &a.relations, &b.relations, &mut out);
    declarations("named_value", &a.named_values, &b.named_values, &mut out);
    let mut old: Vec<_> = a.sites.iter().map(Some).collect();
    let mut new: Vec<_> = b.sites.iter().map(Some).collect();
    // Cancel equal descriptors as a multiset. Prefer nearby copies before
    // matching moves across files. File/line/column are not semantic IDs.
    for locality in [2, 1, 0] {
        let key = |s: &Site| {
            Some(
                json!([
                    (locality > 0).then_some((&s.source.module, &s.source.file)),
                    (locality == 2).then_some((s.source.line, s.source.column)),
                    s.descriptor
                ])
                .to_string(),
            )
        };
        let rhs = groups(&new, &key);
        for (k, lhs) in groups(&old, &key) {
            if let Some(right) = rhs.get(&k) {
                for (i, &j) in lhs.into_iter().zip(right) {
                    old[i] = None;
                    new[j] = None;
                }
            }
        }
    }
    // For edits, use two independent anchors: the original wording, or the
    // ordered argument bindings. Only accept mutually unique evidence.
    let mut candidates = BTreeSet::new();
    for by_format in [true, false] {
        let key = |s: &Site| {
            let d = &s.descriptor;
            let anchor = if by_format {
                json!(d.source_format)
            } else {
                if d.arguments.is_empty() {
                    return None;
                }
                json!(
                    d.arguments
                        .iter()
                        .map(|s| (&s.attribute, &s.label))
                        .collect::<Vec<_>>()
                )
            };
            Some(json!([s.source.module, s.source.file, d.kind, anchor]).to_string())
        };
        let rhs = groups(&new, &key);
        for (k, lhs) in groups(&old, &key) {
            if let ([i], Some([j])) = (lhs.as_slice(), rhs.get(&k).map(Vec::as_slice)) {
                candidates.insert((*i, *j));
            }
        }
    }
    let mut old_count = vec![0; old.len()];
    let mut new_count = vec![0; new.len()];
    for &(i, j) in &candidates {
        old_count[i] += 1;
        new_count[j] += 1;
    }
    for (i, j) in candidates {
        if old_count[i] == 1 && new_count[j] == 1 {
            out.extend(site_change(old[i].take(), new[j].take()));
        }
    }
    for s in old.into_iter().flatten() {
        out.extend(site_change(Some(s), None));
    }
    for s in new.into_iter().flatten() {
        out.extend(site_change(None, Some(s)));
    }
    out
}

fn code(s: &str) -> String {
    format!(
        "<code>{}</code>",
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    )
}

fn markdown(out: &mut impl Write, changes: &[Change]) -> io::Result<()> {
    writeln!(out, "## Telemetry schema diff\n")?;
    if changes.is_empty() {
        return writeln!(out, "No semantic changes.");
    }
    for c in changes {
        writeln!(out, "- **{} {}** {}", c.change, c.item, code(&c.name))?;
        for f in &c.fields {
            let value = |v: &Option<Value>| {
                v.as_ref()
                    .map(|v| code(&v.to_string()))
                    .unwrap_or_else(|| "(absent)".into())
            };
            writeln!(
                out,
                "  - {}: {} -> {}",
                if f.path.is_empty() {
                    "descriptor".into()
                } else {
                    code(&f.path)
                },
                value(&f.before),
                value(&f.after)
            )?;
        }
        let locs = if c.after_sources.is_empty() {
            &c.before_sources
        } else {
            &c.after_sources
        };
        for loc in locs {
            writeln!(
                out,
                "  - {}",
                code(&format!("{}:{}:{}", loc.file, loc.line, loc.column))
            )?;
        }
    }
    Ok(())
}
