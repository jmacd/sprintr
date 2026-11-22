// SPDX-License-Identifier: Apache-2.0

use crate::buffer::RecordBuffer;
use crate::entity::{BorrowedArgument, EntityHandle, Record, with_record};
use crate::runtime::runtime;
use crate::statement::Resolved;
use crate::{Definition, Message, RECORD, STATEMENT_PREFIX, THREAD, json_error};
use std::collections::{HashMap, HashSet};
use std::io::{self, Write};

/// Locations are per output *and* thread: a short reference cannot cross either boundary.
#[derive(Default)]
pub(crate) struct StreamState {
    pub started: bool,
    statements: HashSet<u64>,
    definitions: HashSet<usize>,
    // (writing thread, relation index, entity id): native thread IDs can
    // overlap with the IDs assigned to application entities.
    locations: HashMap<(u64, usize, u64), u64>,
}

/// Only first use allocates. Nothing is installed until the whole line is accepted.
#[derive(Default)]
struct Pending<'a> {
    schema: Vec<usize>,
    entities: Vec<&'a EntityHandle>,
}

impl<'a> Pending<'a> {
    fn schema(&mut self, state: &StreamState, index: usize) {
        if !state.definitions.contains(&index) && !self.schema.contains(&index) {
            self.schema.push(index);
        }
    }

    fn entity(&mut self, state: &StreamState, thread: u64, entity: &'a EntityHandle) {
        if !state
            .locations
            .contains_key(&(thread, entity.relation, entity.id))
            && !self
                .entities
                .iter()
                .any(|e| e.relation == entity.relation && e.id == entity.id)
        {
            self.schema(state, entity.relation);
            self.entities.push(entity);
        }
    }

    fn commit(self, state: &mut StreamState, record: Record<'_>) {
        state.definitions.extend(self.schema);
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
        args: &[BorrowedArgument<'_>],
    ) -> io::Result<()> {
        with_record(|record| {
            let first = !self.statements.contains(&statement.id);
            let mut pending = Pending::default();
            if first {
                for &index in &statement.definitions {
                    pending.schema(self, index);
                }
            }
            let thread = record.thread.id;
            pending.schema(self, record.record_relation);
            pending.entity(self, thread, record.thread);
            for arg in args {
                if let BorrowedArgument::Entity(entity) = arg {
                    pending.entity(self, thread, entity);
                }
            }
            buffer.clear();
            buffer.try_atomic(|buf| {
                write!(
                    buf,
                    "{{\"seq\":{},\"stmt\":{},\"thread\":{thread},\"args\":[",
                    record.seq, statement.id
                )?;
                for (i, arg) in args.iter().enumerate() {
                    if i != 0 {
                        buf.write_all(b",")?;
                    }
                    match arg {
                        BorrowedArgument::String(s) => {
                            serde_json::to_writer(&mut *buf, s).map_err(json_error)?
                        }
                        BorrowedArgument::Unsigned(n) => {
                            serde_json::to_writer(&mut *buf, n).map_err(json_error)?
                        }
                        BorrowedArgument::Entity(e) => {
                            let seq = self
                                .locations
                                .get(&(thread, e.relation, e.id))
                                .copied()
                                .unwrap_or(record.seq);
                            write!(buf, "{{\"$ref\":\"{seq}{}\"}}", e.fragment)?;
                        }
                    }
                }
                buf.write_all(b"]")?;
                if first || !pending.entities.is_empty() {
                    buf.write_all(b",\"defs\":{")?;
                    let mut sep: &[u8] = b"";
                    for &index in &pending.schema {
                        buf.write_all(sep)?;
                        buf.write_all(&runtime().definitions[index].bytes)?;
                        sep = b",";
                    }
                    for e in &pending.entities {
                        buf.write_all(sep)?;
                        serde_json::to_writer(&mut *buf, &e.key).map_err(json_error)?;
                        buf.write_all(b":")?;
                        serde_json::to_writer(&mut *buf, &e.object).map_err(json_error)?;
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
                self.statements.insert(statement.id);
            }
            pending.commit(self, record);
            Ok(())
        })
    }

    pub fn write_snapshot(
        &mut self,
        writer: &mut impl Write,
        buffer: &mut impl RecordBuffer,
        snapshot: &Message,
    ) -> io::Result<()> {
        with_record(|record| {
            // Snapshot arguments use fragment-only references. They still point
            // into this record after we give it a delivery address.
            let mut event = snapshot.clone();
            event.attrs.insert(THREAD.into(), record.thread.id);
            event.attrs.insert(RECORD.into(), record.seq);
            let mut pending = Pending::default();
            pending.schema(self, record.record_relation);
            pending.entity(self, record.thread.id, record.thread);
            for &index in &pending.schema {
                let schema = &runtime().definitions[index];
                event
                    .defs
                    .insert(schema.key.clone(), schema.definition.clone());
            }
            for e in &pending.entities {
                event
                    .defs
                    .insert(e.key.clone(), Definition::Entity(e.object.clone()));
            }
            buffer.clear();
            buffer.try_atomic(|buf| {
                serde_json::to_writer(&mut *buf, &event).map_err(json_error)?;
                buf.write_all(b"\n")
            })?;
            writer.write_all(buffer.as_slice())?;
            pending.commit(self, record);
            Ok(())
        })
    }
}
