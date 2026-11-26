// SPDX-License-Identifier: Apache-2.0

use super::{
    CompiledStatement, Definition, Message, RecordAddress, RelationDefinition, Renderer,
    format::FormatError, reference, span::RecordHistory,
};
use std::{
    collections::BTreeMap,
    fmt::{self, Write as _},
    num::NonZeroU64,
};

pub const EXCEPTION: &str = "exception";
pub const EXCEPTION_CLASS: &str = "exception.class";
pub const EXCEPTION_CAUSE: &str = "exception.cause";

/// A process-local entity UID and, only after delivery, its definition's origin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExceptionTarget {
    pub id: NonZeroU64,
    pub origin: Option<RecordAddress>,
}

impl fmt::Display for ExceptionTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(a) = self.origin {
            write!(f, "log.thread:{}/{}#/defs/", a.thread(), a.sequence())?;
        }
        write!(f, "{EXCEPTION}:{}", self.id)
    }
}

impl ExceptionTarget {
    pub fn parse(text: &str) -> Result<Self, FormatError> {
        let invalid = || FormatError(format!("invalid exception reference {text:?}"));
        let (origin, key) = if let Some(target) = text.strip_prefix("log.thread:") {
            let (thread, target) = target.split_once('/').ok_or_else(invalid)?;
            let (seq, key) = target.split_once("#/defs/").ok_or_else(invalid)?;
            (
                Some(
                    RecordAddress::new(
                        reference::unsigned_id(thread).ok_or_else(invalid)?,
                        reference::unsigned_id(seq).ok_or_else(invalid)?,
                    )
                    .ok_or_else(invalid)?,
                ),
                key,
            )
        } else {
            (None, text)
        };
        let id = key
            .strip_prefix("exception:")
            .and_then(reference::positive_id)
            .and_then(NonZeroU64::new)
            .ok_or_else(invalid)?;
        Ok(Self { id, origin })
    }

    pub fn diagnostic(self) -> impl fmt::Display {
        fmt::from_fn(move |f| {
            if self.origin.is_some() {
                write!(f, "{self}")
            } else {
                write!(f, "<unrecorded {self}>")
            }
        })
    }
}

pub(super) struct Occurrence {
    target: ExceptionTarget,
    statement: u64,
    body: String,
    stack: Vec<u64>,
    cause: Option<ExceptionTarget>,
}

impl Occurrence {
    pub fn read<'a>(
        kind: Option<&str>,
        statement: &CompiledStatement,
        event: &Message,
        address: Option<RecordAddress>,
        body: &str,
        relation: impl Fn(&str) -> Option<&'a RelationDefinition>,
    ) -> Result<Option<Self>, FormatError> {
        let mut defs = event
            .defs
            .iter()
            .filter(|(k, _)| k.starts_with("exception:"));
        let def = defs.next();
        if kind != Some("EXCEPTION") {
            if def.is_some() {
                return Err(FormatError(
                    "exception definitions require an EXCEPTION statement".into(),
                ));
            }
            return Ok(None);
        }
        let Some((key, Definition::Entity(object))) = def else {
            return Err(FormatError(
                "EXCEPTION requires its occurrence definition".into(),
            ));
        };
        if defs.next().is_some() || !statement.template.attrs.contains_key(EXCEPTION_CLASS) {
            return Err(FormatError(
                "EXCEPTION requires one occurrence and a class".into(),
            ));
        }
        let mut target = ExceptionTarget::parse(key)?;
        target.origin = Some(
            address.ok_or_else(|| FormatError("EXCEPTION requires an addressed record".into()))?,
        );
        let cause = match object.get(EXCEPTION_CAUSE) {
            None if object.is_empty() => None,
            Some(value) if object.len() == 1 && relation(EXCEPTION_CAUSE).is_some() => {
                let reference: super::ContextRef = serde_json::from_value(value.clone())
                    .map_err(|e| FormatError(e.to_string()))?;
                Some(ExceptionTarget::parse(&reference.entity)?)
            }
            _ => {
                return Err(FormatError(
                    "invalid exception occurrence properties".into(),
                ));
            }
        };
        Ok(Some(Self {
            target,
            statement: event.attrs[super::STATEMENT],
            body: body.into(),
            stack: event.stack.clone(),
            cause,
        }))
    }
}

#[derive(Default)]
pub(super) struct ExceptionIndex {
    // Include origins mentioned by other threads before their definitions arrive.
    by_id: BTreeMap<NonZeroU64, RecordAddress>,
    by_origin: BTreeMap<RecordAddress, (NonZeroU64, Option<Occurrence>)>,
}

impl ExceptionIndex {
    pub fn validate(
        &self,
        here: Option<RecordAddress>,
        occurrence: Option<&Occurrence>,
        refs: &[ExceptionTarget],
        history: &RecordHistory,
    ) -> Result<(), FormatError> {
        if here.is_some_and(|a| self.by_origin.contains_key(&a)) && occurrence.is_none() {
            return Err(FormatError(
                "expected an exception occurrence at this address".into(),
            ));
        }
        let mut ids = BTreeMap::new();
        let mut origins = BTreeMap::new();
        for (&target, definition) in refs
            .iter()
            .chain(occurrence.and_then(|e| e.cause.as_ref()))
            .map(|t| (t, false))
            .chain(occurrence.map(|e| (&e.target, true)))
        {
            let Some(addr) = target.origin else { continue };
            if !definition
                && here
                    .is_some_and(|a| a.thread() == addr.thread() && a.sequence() <= addr.sequence())
            {
                return Err(FormatError(format!(
                    "exception reference must precede this record: {target}"
                )));
            }
            if history.contains(addr) && !self.by_origin.contains_key(&addr)
                || self
                    .by_origin
                    .get(&addr)
                    .is_some_and(|(id, _)| *id != target.id)
                || self.by_id.get(&target.id).is_some_and(|&a| a != addr)
                || origins
                    .insert(addr, target.id)
                    .is_some_and(|id| id != target.id)
                || ids.insert(target.id, addr).is_some_and(|a| a != addr)
            {
                return Err(FormatError(format!(
                    "conflicting exception origin {target}"
                )));
            }
        }
        // A late definition may close a cross-thread cycle. A new, unclaimed
        // origin cannot already occur in another recorded cause chain.
        if let Some(e) = occurrence {
            let mut next = e.cause;
            let late = here.is_some_and(|a| self.by_origin.contains_key(&a));
            while let Some(t) = next {
                if t.id == e.target.id {
                    return Err(FormatError("cycle in exception causes".into()));
                }
                if !late {
                    break;
                }
                next = t
                    .origin
                    .and_then(|a| self.by_origin.get(&a)?.1.as_ref()?.cause);
            }
        }
        Ok(())
    }

    pub fn commit(&mut self, occurrence: Option<Occurrence>, refs: Vec<ExceptionTarget>) {
        for t in refs
            .into_iter()
            .chain(occurrence.as_ref().and_then(|e| e.cause))
        {
            if let Some(a) = t.origin {
                self.by_id.insert(t.id, a);
                self.by_origin.entry(a).or_insert((t.id, None));
            }
        }
        if let Some(e) = occurrence {
            let a = e.target.origin.expect("validated occurrence address");
            self.by_id.insert(e.target.id, a);
            self.by_origin.insert(a, (e.target.id, Some(e)));
        }
    }
}

impl Renderer {
    /// Expand a captured exception after ingesting its records. `symbol` runs
    /// only here, with the originating process's raw PC; `None` preserves hex.
    pub fn exception_chain(
        &self,
        origin: RecordAddress,
        mut symbol: impl FnMut(u64) -> Option<String>,
    ) -> Option<String> {
        self.try_exception_chain(origin, |pc| Ok::<_, std::convert::Infallible>(symbol(pc)))
            .expect("infallible symbol callback")
    }

    /// As `exception_chain`, but distinguish absent symbols from resolver errors.
    pub fn try_exception_chain<E>(
        &self,
        origin: RecordAddress,
        mut symbol: impl FnMut(u64) -> Result<Option<String>, E>,
    ) -> Result<Option<String>, E> {
        let Some(mut e) = self
            .exceptions
            .by_origin
            .get(&origin)
            .and_then(|(_, e)| e.as_ref())
        else {
            return Ok(None);
        };
        let mut out = String::new();
        let mut previous: &[u64] = &[];
        loop {
            let template = self
                .statement(e.statement)
                .expect("accepted exception statement");
            write!(
                out,
                "{}: {}\n    {}",
                template.attrs[EXCEPTION_CLASS], e.body, template.source
            )
            .expect("write to string");
            let common = previous
                .iter()
                .rev()
                .zip(e.stack.iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            for &pc in &e.stack[..e.stack.len() - common] {
                if let Some(frame) = symbol(pc)? {
                    write!(out, "\n    {frame}")
                } else {
                    write!(out, "\n    at <unresolved> ({pc:#x})")
                }
                .expect("write to string");
            }
            if common > 0 {
                write!(out, "\n    ... {common} more").expect("write to string");
            }
            let Some(cause) = e.cause else { break };
            out.push_str("\nCaused by: ");
            let Some(next) = cause
                .origin
                .and_then(|a| self.exceptions.by_origin.get(&a)?.1.as_ref())
            else {
                if cause.origin.is_some() {
                    write!(out, "<missing {cause}>")
                } else {
                    write!(out, "{}", cause.diagnostic())
                }
                .expect("write to string");
                break;
            };
            previous = &e.stack;
            e = next;
        }
        Ok(Some(out))
    }
}
