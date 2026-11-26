// SPDX-License-Identifier: Apache-2.0

use crate::buffer::RecordBuffer;
use crate::context::{self, ContextDefinition};
use crate::entity::{EntityHandle, Record, with_record};
use crate::runtime::runtime;
use crate::statement::{EncoderCache, NamedEncoder, Resolved};
use crate::structured::{Encoder, Scratch};
use crate::{EventArgs, EventKind, RecordAddress, STACK, STATEMENT_PREFIX, json_error, schema};
use std::collections::{HashMap, HashSet};
use std::io::{self, Write};

/// Locations are per output *and* thread: a short reference cannot cross either boundary.
#[derive(Default)]
pub(crate) struct StreamState {
    pub startup: Option<io::Result<()>>,
    accepted: Accepted,
    scratch: Scratch,
    encoders: EncoderCache,
}

#[derive(Default)]
pub(crate) struct Accepted {
    statements: HashSet<u64>,
    definitions: HashSet<usize>,
    encoders: HashSet<(u64, usize)>,
    // (writing thread, relation index, entity id): native thread IDs can
    // overlap with the IDs assigned to application entities.
    locations: HashMap<(u64, usize, u64), u64>,
    // Independently constructed handles can name the same logical entity.
    // Remember one representative and compare its immutable bytes on first use
    // of another handle, never on a warmed handle's numeric location hit.
    named: HashMap<String, (u64, Box<[u8]>)>,
}

/// Only first use allocates. Nothing is installed until the whole line is accepted.
#[derive(Default)]
pub(crate) struct Pending {
    schema: Vec<usize>,
    encoders: Vec<std::sync::Arc<NamedEncoder>>,
    // Keep first-use payloads alive even when a formatter creates a temporary.
    entities: Vec<EntityHandle>,
    aliases: Vec<(usize, u64, u64)>,
}

impl Pending {
    pub(crate) fn encoder(
        &mut self,
        state: &Accepted,
        thread: u64,
        encoder: &std::sync::Arc<NamedEncoder>,
    ) {
        if !state.encoders.contains(&(thread, encoder.id))
            && !self.encoders.iter().any(|e| e.id == encoder.id)
        {
            self.encoders.push(encoder.clone());
        }
    }

    pub(crate) fn schema(&mut self, state: &Accepted, index: usize) {
        if !state.definitions.contains(&index) && !self.schema.contains(&index) {
            for &dep in &runtime().definitions[index].dependencies {
                self.schema(state, dep);
            }
            self.schema.push(index);
        }
    }

    pub(crate) fn entity(
        &mut self,
        state: &Accepted,
        record: Record<'_>,
        entity: &EntityHandle,
    ) -> io::Result<u64> {
        if let Some(&seq) = state
            .locations
            .get(&(record.thread.id, entity.relation, entity.id))
        {
            return Ok(seq);
        }
        let payload = &entity.payload;
        let conflict = || {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("conflicting definition for {:?}", payload.key),
            )
        };
        if entity.is_named()
            && let Some((id, member)) = state.named.get(&payload.key)
        {
            if *member != payload.member {
                return Err(conflict());
            }
            if let Some(&seq) = state
                .locations
                .get(&(record.thread.id, entity.relation, *id))
            {
                self.aliases.push((entity.relation, entity.id, seq));
                return Ok(seq);
            }
        }
        if let Some(previous) = self.entities.iter().find(|e| {
            e.relation == entity.relation && (e.id == entity.id || e.payload.key == payload.key)
        }) {
            if previous.id != entity.id {
                if previous.payload.member != payload.member {
                    return Err(conflict());
                }
                self.aliases.push((entity.relation, entity.id, record.seq));
            }
            return Ok(record.seq);
        }
        self.schema(state, entity.relation);
        self.entities.push(entity.clone());
        Ok(record.seq)
    }

    fn commit(self, state: &mut Accepted, record: Record<'_>) {
        state.definitions.extend(self.schema);
        state
            .encoders
            .extend(self.encoders.into_iter().map(|e| (record.thread.id, e.id)));
        for entity in self.entities {
            if entity.is_named() {
                let (canonical, _) = state
                    .named
                    .entry(entity.payload.key.clone())
                    .or_insert_with(|| (entity.id, entity.payload.member.clone()));
                state
                    .locations
                    .insert((record.thread.id, entity.relation, *canonical), record.seq);
            }
            state
                .locations
                .insert((record.thread.id, entity.relation, entity.id), record.seq);
        }
        for (relation, id, seq) in self.aliases {
            state
                .locations
                .insert((record.thread.id, relation, id), seq);
        }
    }
}

impl StreamState {
    pub fn write_compiled(
        &mut self,
        writer: &mut impl Write,
        buffer: &mut impl RecordBuffer,
        statement: &Resolved,
        stack: &[u64],
        args: EventArgs<'_>,
    ) -> io::Result<RecordAddress> {
        with_record(|record| {
            let address = RecordAddress::new(record.thread.id, record.seq)
                .ok_or_else(|| io::Error::other("native thread ID must be nonzero"))?;
            self.write_record(writer, buffer, statement, stack, args, record)?;
            Ok(address)
        })
    }

    pub(crate) fn write_record(
        &mut self,
        writer: &mut impl Write,
        buffer: &mut impl RecordBuffer,
        statement: &Resolved,
        stack: &[u64],
        args: EventArgs<'_>,
        record: Record<'_>,
    ) -> io::Result<()> {
        let timestamp = crate::timestamp_now()?;
        context::with_current(|cx| {
            let state = &mut self.accepted;
            let first = !state.statements.contains(&statement.id);
            let mut pending = Pending::default();
            let occurrence = if let EventKind::Exception(e) = args.kind {
                let relation = if e.cause.is_some() {
                    schema::exception_cause.__meta()
                } else {
                    schema::exception.__meta()
                };
                pending.schema(
                    state,
                    runtime().relation(relation).map_err(io::Error::other)?,
                );
                Some(e)
            } else {
                None
            };
            if first {
                for &index in &statement.definitions {
                    pending.schema(state, index);
                }
            }
            let thread = record.thread.id;
            pending.schema(state, record.record_relation);
            pending.entity(state, record, record.thread)?;
            self.scratch.clear();
            buffer.clear();
            buffer.try_atomic(|buf| {
                write!(
                    buf,
                    "{{\"seq\":{},\"stmt\":{},\"thread\":{thread},\"ts\":{timestamp},\"attrs\":{{\"{STACK}\":",
                    record.seq, statement.id
                )?;
                serde_json::to_writer(&mut *buf, stack).map_err(json_error)?;
                for binding in cx.entries() {
                    let binding = binding.data();
                    pending.schema(state, binding.attribute);
                    match &binding.definition {
                        ContextDefinition::Named { index, .. } => {
                            pending.schema(state, binding.relation);
                            pending.schema(state, *index);
                        }
                        ContextDefinition::Handle(handle) => {
                            pending.entity(state, record, handle)?;
                        }
                    }
                    buf.write_all(b",")?;
                    buf.write_all(&binding.member)?;
                }
                buf.write_all(b"},\"args\":[")?;
                if matches!(args.kind, EventKind::SpanStart(_)) || cx.span().is_some() {
                    for meta in [schema::span_start.__meta(), schema::trace_start.__meta()] {
                        pending.schema(state, runtime().relation(meta).map_err(io::Error::other)?);
                    }
                    if let EventKind::SpanStart(start) = args.kind {
                        if start.has_parent() {
                            pending.schema(state, runtime().relation(schema::span_parent.__meta()).map_err(io::Error::other)?);
                        }
                        let address = RecordAddress::new(thread, record.seq).ok_or_else(|| io::Error::other("invalid thread ID"))?;
                        start.write_refs(address, &mut self.scratch.refs)?;
                    } else if let Some(span) = cx.span() {
                        span.write_membership(&mut self.scratch.refs);
                    }
                }
                Encoder::new(buf, &mut self.scratch, &mut self.encoders, state, &mut pending, record).args(args.values)?;
                buf.write_all(b"]")?;
                if !self.scratch.refs.is_empty() {
                    buf.write_all(b",\"refs\":[")?;
                    buf.write_all(&self.scratch.refs)?;
                    buf.write_all(b"]")?;
                }
                if first || occurrence.is_some() || !pending.schema.is_empty() || !pending.entities.is_empty() || !pending.encoders.is_empty() {
                    buf.write_all(b",\"defs\":{")?;
                    let mut sep: &[u8] = b"";
                    if let Some(e) = occurrence {
                        e.write_definition(buf)?;
                        sep = b",";
                    }
                    for &index in &pending.schema {
                        buf.write_all(sep)?;
                        buf.write_all(&runtime().definitions[index].bytes)?;
                        sep = b",";
                    }
                    for entity in &pending.entities {
                        buf.write_all(sep)?;
                        buf.write_all(&entity.payload.member)?;
                        sep = b",";
                    }
                    for encoder in &pending.encoders {
                        buf.write_all(sep)?;
                        buf.write_all(&encoder.member)?;
                        sep = b",";
                    }
                    if first {
                        buf.write_all(sep)?;
                        write!(buf, "\"{STATEMENT_PREFIX}{}\":", statement.id)?;
                        buf.write_all(&statement.template)?;
                    }
                    buf.write_all(b"}")?;
                }
                buf.write_all(b"}\n")
            })?;
            writer.write_all(buffer.as_slice())?;
            if first {
                state.statements.insert(statement.id);
            }
            pending.commit(state, record);
            Ok(())
        })
    }
}
