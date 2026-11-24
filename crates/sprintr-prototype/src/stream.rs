// SPDX-License-Identifier: Apache-2.0

use crate::buffer::RecordBuffer;
use crate::context::{self, ContextDefinition};
use crate::entity::{BorrowedArgument, EntityHandle, Record, with_record};
use crate::runtime::runtime;
use crate::statement::{EncoderCache, NamedEncoder, Resolved};
use crate::structured::{Encoder, Scratch};
use crate::{RecordAddress, STACK, STATEMENT_PREFIX, json_error};
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
}

/// Only first use allocates. Nothing is installed until the whole line is accepted.
#[derive(Default)]
pub(crate) struct Pending {
    schema: Vec<usize>,
    encoders: Vec<std::sync::Arc<NamedEncoder>>,
    // Keep first-use payloads alive even when a formatter creates a temporary.
    entities: Vec<EntityHandle>,
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
            self.schema.push(index);
        }
    }

    pub(crate) fn entity(
        &mut self,
        state: &Accepted,
        record: Record<'_>,
        entity: &EntityHandle,
    ) -> u64 {
        if let Some(&seq) = state
            .locations
            .get(&(record.thread.id, entity.relation, entity.id))
        {
            return seq;
        }
        if !self
            .entities
            .iter()
            .any(|e| e.relation == entity.relation && e.id == entity.id)
        {
            self.schema(state, entity.relation);
            self.entities.push(entity.clone());
        }
        record.seq
    }

    fn commit(self, state: &mut Accepted, record: Record<'_>) {
        state.definitions.extend(self.schema);
        state
            .encoders
            .extend(self.encoders.into_iter().map(|e| (record.thread.id, e.id)));
        for entity in self.entities {
            state
                .locations
                .insert((record.thread.id, entity.relation, entity.id), record.seq);
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
        args: &[BorrowedArgument<'_>],
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
        args: &[BorrowedArgument<'_>],
        record: Record<'_>,
    ) -> io::Result<()> {
        let timestamp = crate::timestamp_now()?;
        context::with_current(|bindings| {
            let state = &mut self.accepted;
            let first = !state.statements.contains(&statement.id);
            let mut pending = Pending::default();
            if first {
                for &index in &statement.definitions {
                    pending.schema(state, index);
                }
            }
            let thread = record.thread.id;
            pending.schema(state, record.record_relation);
            pending.entity(state, record, record.thread);
            self.scratch.clear();
            buffer.clear();
            buffer.try_atomic(|buf| {
                write!(
                    buf,
                    "{{\"seq\":{},\"stmt\":{},\"thread\":{thread},\"ts\":{timestamp},\"attrs\":{{\"{STACK}\":",
                    record.seq, statement.id
                )?;
                serde_json::to_writer(&mut *buf, stack).map_err(json_error)?;
                for binding in bindings {
                    let binding = binding.data();
                    pending.schema(state, binding.attribute);
                    match &binding.definition {
                        ContextDefinition::Named { index, .. } => {
                            pending.schema(state, binding.relation);
                            pending.schema(state, *index);
                        }
                        ContextDefinition::Anonymous(handle) => {
                            pending.entity(state, record, handle);
                        }
                    }
                    buf.write_all(b",")?;
                    buf.write_all(&binding.member)?;
                }
                buf.write_all(b"},\"args\":[")?;
                Encoder::new(buf, &mut self.scratch, &mut self.encoders, state, &mut pending, record).args(args)?;
                buf.write_all(b"]")?;
                if !self.scratch.refs.is_empty() {
                    buf.write_all(b",\"refs\":[")?;
                    buf.write_all(&self.scratch.refs)?;
                    buf.write_all(b"]")?;
                }
                if first || !pending.schema.is_empty() || !pending.entities.is_empty() || !pending.encoders.is_empty() {
                    buf.write_all(b",\"defs\":{")?;
                    let mut sep: &[u8] = b"";
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
