// SPDX-License-Identifier: Apache-2.0

use super::{Message, RecordAddress, Renderer, format::FormatError};
use std::collections::BTreeMap;
use std::num::{NonZeroU64, NonZeroU128};

/// Names the start record of the current span by 16-digit lowercase span ID.
pub const SPAN_START: &str = "span.start";
/// Names the root start record by 32-digit lowercase trace ID.
pub const TRACE_START: &str = "trace.start";
/// Names the immediate parent's start record on a child start.
pub const SPAN_PARENT: &str = "span.parent";

/// A span ID and the process address of its start, not necessarily in this output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpanLink {
    pub id: NonZeroU64,
    pub address: RecordAddress,
}

/// A trace ID and the process address of its root span's start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TraceLink {
    pub id: NonZeroU128,
    pub address: RecordAddress,
}

/// Membership recorded on this event. To find the parent of an ordinary
/// member's span, look up `current.address` with [`Renderer::span_start`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpanLinks {
    pub current: SpanLink,
    pub root: TraceLink,
    /// Present only on the start of a non-root span.
    pub parent: Option<SpanLink>,
}

impl SpanLinks {
    /// All addresses asserted to start a span in `self.root`'s trace. A trace
    /// link on its own doesn't tell us the root's span ID.
    fn targets(self) -> impl Iterator<Item = (RecordAddress, Option<NonZeroU64>)> {
        std::iter::once(self.current)
            .chain(self.parent)
            .map(|s| (s.address, Some(s.id)))
            .chain([(self.root.address, None)])
    }
}

impl Message {
    /// Parse the span annotations without resolving them or interpreting the statement.
    /// Use [`Renderer::render`] to validate them against the statement kind and stream.
    pub fn span_links(&self) -> Result<Option<SpanLinks>, FormatError> {
        let mut refs = SpanRefs::default();
        for entry in self.event_refs() {
            let (key, target) = entry?;
            refs.collect(key, target)?;
        }
        refs.finish()
    }
}

impl Renderer {
    /// Metadata of a start already accepted at `address`. An otherwise valid link
    /// to a start in another output may remain unresolved.
    pub fn span_start(&self, address: RecordAddress) -> Option<SpanLinks> {
        self.spans.targets.get(&address)?.start
    }
}

#[derive(Default)]
pub(super) struct SpanRefs {
    current: Option<SpanLink>,
    root: Option<TraceLink>,
    parent: Option<SpanLink>,
}

pub(super) fn marker_id(relation: &str, id: &str) -> Result<Option<NonZeroU128>, FormatError> {
    let width = match relation {
        SPAN_START | SPAN_PARENT => 16,
        TRACE_START => 32,
        _ => return Ok(None),
    };
    let invalid = || FormatError(format!("invalid {width}-digit ID in {relation}:{id}"));
    if id.len() != width
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid());
    }
    u128::from_str_radix(id, 16)
        .ok()
        .and_then(NonZeroU128::new)
        .map(Some)
        .ok_or_else(invalid)
}

impl SpanRefs {
    /// Returns false for a non-span association (which has its own target grammar).
    pub fn collect(&mut self, key: &str, target: &str) -> Result<bool, FormatError> {
        let (relation, id) = key.split_once(':').unwrap_or((key, ""));
        let Some(id) = marker_id(relation, id)? else {
            return Ok(false);
        };
        let address = RecordAddress::parse_span_start_ref(target)?;
        let duplicate = if relation == TRACE_START {
            self.root.replace(TraceLink { id, address }).is_some()
        } else {
            let id = NonZeroU64::try_from(id).expect("16-digit span ID");
            let slot = if relation == SPAN_START {
                &mut self.current
            } else {
                &mut self.parent
            };
            slot.replace(SpanLink { id, address }).is_some()
        };
        if duplicate {
            return Err(FormatError(format!("duplicate {relation} annotation")));
        }
        Ok(true)
    }

    pub fn finish(self) -> Result<Option<SpanLinks>, FormatError> {
        match (self.current, self.root, self.parent) {
            (None, None, None) => Ok(None),
            (Some(current), Some(root), parent) => Ok(Some(SpanLinks {
                current,
                root,
                parent,
            })),
            _ => Err(FormatError(format!(
                "span annotations require both {SPAN_START} and {TRACE_START}"
            ))),
        }
    }
}

/// A gap may belong to another output. Only actual accepted records rule out
/// a missing start. Consecutive records on a thread cost a single range.
#[derive(Default)]
pub(super) struct RecordHistory(BTreeMap<u64, Vec<(u64, u64)>>);

impl RecordHistory {
    pub fn increases(&self, address: RecordAddress) -> bool {
        self.0
            .get(&address.thread())
            .and_then(|runs| runs.last())
            .is_none_or(|&(_, end)| address.sequence() > end)
    }

    pub(super) fn contains(&self, address: RecordAddress) -> bool {
        let Some(runs) = self.0.get(&address.thread()) else {
            return false;
        };
        let seq = address.sequence();
        let i = runs.partition_point(|&(_, end)| end < seq);
        runs.get(i).is_some_and(|&(begin, _)| begin <= seq)
    }

    pub fn record(&mut self, address: RecordAddress) {
        let runs = self.0.entry(address.thread()).or_default();
        let seq = address.sequence();
        if let Some((_, end)) = runs.last_mut()
            && end.checked_add(1) == Some(seq)
        {
            *end = seq;
        } else {
            runs.push((seq, seq));
        }
    }
}

#[derive(Default)]
struct StartTarget {
    span_id: Option<NonZeroU64>,
    trace: Option<TraceLink>,
    start: Option<SpanLinks>,
    ended: bool,
}

/// Include claims made by other records, even if the corresponding start
/// hasn't been read. A later start must agree, not silently replace the claim.
#[derive(Default)]
pub(super) struct SpanIndex {
    targets: BTreeMap<RecordAddress, StartTarget>,
    span_ids: BTreeMap<(NonZeroU128, NonZeroU64), RecordAddress>,
    trace_ids: BTreeMap<NonZeroU128, RecordAddress>,
}

#[derive(Clone, Copy)]
pub(super) enum SpanEvent {
    Start(SpanLinks),
    End(SpanLinks),
    Member(SpanLinks),
    Unlinked,
}

impl SpanEvent {
    pub fn is_start(self) -> bool {
        matches!(self, Self::Start(_))
    }

    fn links(self) -> Option<SpanLinks> {
        match self {
            Self::Start(links) | Self::End(links) | Self::Member(links) => Some(links),
            Self::Unlinked => None,
        }
    }

    pub fn from_record(
        kind: Option<&str>,
        address: Option<RecordAddress>,
        links: Option<SpanLinks>,
    ) -> Result<Self, FormatError> {
        let Some(links) = links else {
            return match kind {
                Some(marker @ ("SPAN_START" | "SPAN_END")) => Err(FormatError(format!(
                    "{marker} requires span and trace links"
                ))),
                _ => Ok(Self::Unlinked),
            };
        };
        let addr = address
            .ok_or_else(|| FormatError("span annotations require an addressed record".into()))?;
        if kind == Some("SPAN_START") {
            if links.current.address != addr {
                return Err(FormatError(
                    "SPAN_START must name this record as the current span start".into(),
                ));
            }
            if (links.root.address != addr) != links.parent.is_some() {
                return Err(FormatError(
                    "only a non-root SPAN_START requires a parent".into(),
                ));
            }
            if links
                .parent
                .is_some_and(|p| p.address == addr || p.id == links.current.id)
            {
                return Err(FormatError("a span cannot be its own parent".into()));
            }
            return Ok(Self::Start(links));
        }
        if links.parent.is_some() {
            return Err(FormatError(
                "only a child SPAN_START may have a parent annotation".into(),
            ));
        }
        Ok(if kind == Some("SPAN_END") {
            Self::End(links)
        } else {
            Self::Member(links)
        })
    }
}

impl SpanIndex {
    fn check_target(
        &self,
        target: RecordAddress,
        origin: Option<RecordAddress>,
        allow_self: bool,
        history: &RecordHistory,
    ) -> Result<(), FormatError> {
        if let Some(origin) = origin
            && target.thread() == origin.thread()
            && (target.sequence() > origin.sequence() || (target == origin && !allow_self))
        {
            return Err(FormatError(format!(
                "span link to {} must name an earlier start on this thread",
                target.span_start_ref()
            )));
        }
        if history.contains(target) && self.targets.get(&target).is_none_or(|t| t.start.is_none()) {
            return Err(FormatError(format!(
                "{} is not a span start",
                target.span_start_ref()
            )));
        }
        Ok(())
    }

    pub fn validate_inline(
        &self,
        targets: &[RecordAddress],
        origin: Option<RecordAddress>,
        start: bool,
        history: &RecordHistory,
    ) -> Result<(), FormatError> {
        for &target in targets {
            self.check_target(target, origin, start, history)?;
        }
        Ok(())
    }

    pub fn validate(
        &self,
        event: SpanEvent,
        address: Option<RecordAddress>,
        history: &RecordHistory,
    ) -> Result<(), FormatError> {
        if !event.is_start()
            && let Some(addr) = address
            && self.targets.contains_key(&addr)
        {
            return Err(FormatError(format!(
                "expected SPAN_START at {}",
                addr.span_start_ref()
            )));
        }
        let Some(links) = event.links() else {
            return Ok(());
        };
        if self
            .trace_ids
            .get(&links.root.id)
            .is_some_and(|&a| a != links.root.address)
        {
            return Err(FormatError(format!(
                "conflicting start address for trace {:032x}",
                links.root.id
            )));
        }
        // Every referenced span, including the parent and the root itself, must
        // belong to this same trace, whether its start arrives before or after us.
        for (target, span_id) in links.targets() {
            self.check_target(target, address, event.is_start(), history)?;
            if target.thread() == links.root.address.thread()
                && target.sequence() < links.root.address.sequence()
            {
                return Err(FormatError(format!(
                    "{} cannot precede its trace root on this thread",
                    target.span_start_ref()
                )));
            }
            let existing = self.targets.get(&target);
            if existing
                .and_then(|t| t.trace)
                .is_some_and(|trace| trace != links.root)
            {
                return Err(FormatError(format!(
                    "conflicting trace at {}",
                    target.span_start_ref()
                )));
            }
            if let Some(id) = span_id
                && (self
                    .span_ids
                    .get(&(links.root.id, id))
                    .is_some_and(|&a| a != target)
                    || existing
                        .and_then(|t| t.span_id)
                        .is_some_and(|known| known != id))
            {
                return Err(FormatError(format!(
                    "conflicting span ID/address pair {id:016x} at {}",
                    target.span_start_ref()
                )));
            }
        }
        // A new address cannot close a cycle. For a previously referenced
        // start arriving late, check the chain of already accepted parents.
        if let Some(mut ancestor) = links.parent
            && self.targets.contains_key(&links.current.address)
        {
            while let Some(next) = self
                .targets
                .get(&ancestor.address)
                .and_then(|t| t.start?.parent)
            {
                if next.address == links.current.address {
                    return Err(FormatError("cycle in span parentage".into()));
                }
                ancestor = next;
            }
        }
        if matches!(event, SpanEvent::End(_))
            && self
                .targets
                .get(&links.current.address)
                .is_some_and(|t| t.ended)
        {
            return Err(FormatError(format!(
                "duplicate SPAN_END for {:016x}",
                links.current.id
            )));
        }
        Ok(())
    }

    pub fn commit(&mut self, event: SpanEvent, inline: Vec<RecordAddress>) {
        for target in inline {
            self.targets.entry(target).or_default();
        }
        let Some(links) = event.links() else {
            return;
        };
        self.trace_ids.insert(links.root.id, links.root.address);
        for (address, span_id) in links.targets() {
            let target = self.targets.entry(address).or_default();
            if let Some(id) = span_id {
                self.span_ids.insert((links.root.id, id), address);
                target.span_id = Some(id);
            }
            target.trace = Some(links.root);
        }
        let current = self
            .targets
            .get_mut(&links.current.address)
            .expect("current link installed");
        match event {
            SpanEvent::Start(_) => current.start = Some(links),
            SpanEvent::End(_) => current.ended = true,
            _ => {}
        }
    }
}
