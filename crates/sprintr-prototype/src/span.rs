// SPDX-License-Identifier: Apache-2.0

use crate::{Context, RecordAddress};
use sprintr_message::{SPAN_PARENT, SPAN_START, TRACE_START};
use std::io::{self, Write};
use std::num::{NonZeroU64, NonZeroU128};
use std::sync::Arc;

/// A transferable reference to an accepted span start. Cloning this reference
/// never extends the span's lifetime.
#[derive(Clone, Debug)]
pub struct SpanRef(Arc<RecordedSpan>);

#[derive(Debug)]
struct RecordedSpan {
    anchors: Anchors,
    membership: Box<[u8]>,
}

#[derive(Clone, Copy, Debug)]
struct Anchors {
    span: NonZeroU64,
    trace: NonZeroU128,
    start: RecordAddress,
    root: RecordAddress,
}

impl Anchors {
    fn write_membership(self, out: &mut Vec<u8>) -> io::Result<()> {
        write!(
            out,
            "{{\"{SPAN_START}:{:016x}\":\"{}\"}},{{\"{TRACE_START}:{:032x}\":\"{}\"}}",
            self.span,
            self.start.span_start_ref(),
            self.trace,
            self.root.span_start_ref()
        )
    }
}

impl SpanRef {
    pub fn span_id(&self) -> NonZeroU64 {
        self.0.anchors.span
    }
    pub fn trace_id(&self) -> NonZeroU128 {
        self.0.anchors.trace
    }
    pub fn start(&self) -> RecordAddress {
        self.0.anchors.start
    }
    pub fn trace_root(&self) -> RecordAddress {
        self.0.anchors.root
    }

    pub(crate) fn write_membership(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0.membership);
    }
}

pub(crate) struct Start {
    span: NonZeroU64,
    trace: NonZeroU128,
    parent: Option<Anchors>,
}

impl Start {
    pub(crate) fn new(parent: Option<&SpanRef>) -> io::Result<Self> {
        let rng_error = |e| io::Error::other(format!("cannot generate tracing ID: {e}"));
        let span = loop {
            if let Some(id) = NonZeroU64::new(getrandom::u64().map_err(rng_error)?)
                && parent.is_none_or(|p| p.span_id() != id)
            {
                break id;
            }
        };
        let trace = match parent {
            Some(p) => p.trace_id(),
            None => loop {
                let mut bytes = [0; 16];
                getrandom::fill(&mut bytes).map_err(rng_error)?;
                if let Some(id) = NonZeroU128::new(u128::from_be_bytes(bytes)) {
                    break id;
                }
            },
        };
        Ok(Self {
            span,
            trace,
            parent: parent.map(|p| p.0.anchors),
        })
    }

    fn anchors(&self, address: RecordAddress) -> Anchors {
        Anchors {
            span: self.span,
            trace: self.trace,
            start: address,
            root: self.parent.map_or(address, |p| p.root),
        }
    }

    pub(crate) fn write_refs(&self, address: RecordAddress, out: &mut Vec<u8>) -> io::Result<()> {
        self.anchors(address).write_membership(out)?;
        if let Some(parent) = self.parent {
            write!(
                out,
                ",{{\"{SPAN_PARENT}:{:016x}\":\"{}\"}}",
                parent.span,
                parent.start.span_start_ref()
            )?;
        }
        Ok(())
    }

    pub(crate) fn has_parent(&self) -> bool {
        self.parent.is_some()
    }

    pub(crate) fn accepted(&self, address: RecordAddress) -> SpanRef {
        let anchors = self.anchors(address);
        let mut membership = Vec::new();
        anchors
            .write_membership(&mut membership)
            .expect("write to Vec");
        SpanRef(Arc::new(RecordedSpan {
            anchors,
            membership: membership.into_boxed_slice(),
        }))
    }
}

/// The single local lifecycle owner returned by `span!`.
#[must_use = "dropping the span closes it"]
pub struct Span {
    context: Context,
}

impl Span {
    pub(crate) fn new(base: Context, reference: Option<SpanRef>) -> Self {
        Self {
            context: base.for_span(reference),
        }
    }

    pub fn context(&self) -> &Context {
        &self.context
    }

    /// Keep a reference for an event on this or another thread.
    pub fn reference(&self) -> Option<SpanRef> {
        self.context.span().cloned()
    }

    pub fn in_scope<R>(&self, f: impl FnOnce() -> R) -> R {
        self.context.in_scope(f)
    }

    /// Close now rather than at the end of the Rust scope.
    pub fn end(self) {
        drop(self);
    }

    /// Transfer the single close right and span anchors, without exporting
    /// thread-local application entities. Dropping `self` no longer closes it.
    pub fn into_owner(mut self) -> SpanOwner {
        SpanOwner(self.context.take_span())
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        if self.context.span().is_some() {
            self.context.in_scope(end_event);
        }
    }
}

/// A movable, non-cloneable lifecycle owner. On activation the destination
/// thread supplies the baggage; the span and trace anchors remain unchanged.
#[must_use = "dropping the owner closes the span"]
pub struct SpanOwner(Option<SpanRef>);

impl SpanOwner {
    pub fn reference(&self) -> Option<SpanRef> {
        self.0.clone()
    }
    pub fn in_scope<R>(&self, f: impl FnOnce() -> R) -> R {
        Context::current().for_span(self.reference()).in_scope(f)
    }
    pub fn end(self) {
        drop(self);
    }
}

impl Drop for SpanOwner {
    fn drop(&mut self) {
        if let Some(span) = self.0.take() {
            Context::current().for_span(Some(span)).in_scope(end_event);
        }
    }
}

fn end_event() {
    crate::__private::report(
        crate::__record!(crate; [crate::schema::log_kind::SPAN_END]; "span ended"),
    );
}
