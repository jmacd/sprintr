// SPDX-License-Identifier: Apache-2.0

use crate::buffer::RecordBuffer;
use crate::entity::{BorrowedArgument, EntityHandle, Record, with_record};
use crate::runtime::runtime;
use crate::statement::{EncoderCache, NamedEncoder, Resolved};
use crate::structured::{Encoder, Scratch};
use crate::{STACK, STATEMENT_PREFIX, json_error};
use std::borrow::Cow;
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
pub(crate) struct Pending<'a> {
    schema: Vec<usize>,
    encoders: Vec<std::sync::Arc<NamedEncoder>>,
    // Root arguments outlive assembly. Only a newly introduced handle supplied
    // by a nested formatter might disappear before we reach the defs section.
    entities: Vec<(usize, u64, Cow<'a, [u8]>)>,
}

impl<'a> Pending<'a> {
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
        member: impl FnOnce() -> Cow<'a, [u8]>,
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
            .any(|(relation, id, _)| *relation == entity.relation && *id == entity.id)
        {
            self.schema(state, entity.relation);
            self.entities.push((entity.relation, entity.id, member()));
        }
        record.seq
    }

    fn commit(self, state: &mut Accepted, record: Record<'_>) {
        state.definitions.extend(self.schema);
        state
            .encoders
            .extend(self.encoders.into_iter().map(|e| (record.thread.id, e.id)));
        for (relation, id, _) in self.entities {
            state
                .locations
                .insert((record.thread.id, relation, id), record.seq);
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
    ) -> io::Result<()> {
        with_record(|record| self.write_record(writer, buffer, statement, stack, args, record))
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
        pending.entity(state, record, record.thread, || {
            Cow::Borrowed(&record.thread.member)
        });
        self.scratch.clear();
        buffer.clear();
        buffer.try_atomic(|buf| {
                write!(
                    buf,
                    "{{\"seq\":{},\"stmt\":{},\"thread\":{thread},\"ts\":{timestamp},\"attrs\":{{\"{STACK}\":",
                    record.seq, statement.id
                )?;
                serde_json::to_writer(&mut *buf, stack).map_err(json_error)?;
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
                    for (_, _, member) in &pending.entities {
                        buf.write_all(sep)?;
                        buf.write_all(member)?;
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
    }
}
