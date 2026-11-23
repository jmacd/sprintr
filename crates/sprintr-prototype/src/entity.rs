// SPDX-License-Identifier: Apache-2.0

use crate::runtime::runtime;
use crate::{Entity, RECORD, SOURCE, STATEMENT, THREAD, schema};
use serde::Serialize;
use serde_json::{Map, Value};
use std::cell::Cell;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

/// An immutable snapshot of an application object. Logging borrows the handle;
/// the first use in each output/thread defines it, later uses refer back to it.
pub struct EntityHandle {
    pub(crate) id: u64,
    pub(crate) relation: usize,
    pub(crate) key: String,
    pub(crate) fragment: String,
    pub(crate) member: Box<[u8]>,
}

impl Entity<true> {
    /// Give a serializable object a new process-local identity. No record is written yet.
    #[allow(clippy::new_ret_no_self, clippy::wrong_self_convention)]
    pub fn new(&'static self, value: impl Serialize) -> io::Result<EntityHandle> {
        let runtime = runtime();
        let invalid = |msg| io::Error::new(io::ErrorKind::InvalidInput, msg);
        let relation = runtime.relation(self.__meta()).map_err(invalid)?;
        let name = &runtime.definitions[relation].name;
        if matches!(name.as_str(), STATEMENT | RECORD | THREAD | SOURCE) {
            return Err(invalid(format!("{name} values are created by the logger")));
        }
        let Value::Object(object) = serde_json::to_value(value).map_err(crate::json_error)? else {
            return Err(invalid(format!("{name} requires a JSON object")));
        };
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| io::Error::other("anonymous entity identities exhausted"))?;
        EntityHandle::with_id(relation, id, object)
    }
}

impl EntityHandle {
    fn with_id(relation: usize, id: u64, object: Map<String, Value>) -> io::Result<Self> {
        let key = format!("{}:{id}", runtime().definitions[relation].name);
        let mut member = serde_json::to_vec(&key).map_err(crate::json_error)?;
        member.push(b':');
        serde_json::to_writer(&mut member, &object).map_err(crate::json_error)?;
        Ok(Self {
            id,
            relation,
            fragment: sprintr_message::definition_fragment(&key),
            key,
            member: member.into(),
        })
    }
}

#[derive(Clone, Copy)]
pub enum BorrowedArgument<'a> {
    String(&'a str),
    Unsigned(u64),
    Entity(&'a EntityHandle),
    Structured(&'a dyn crate::StructuredValue),
    Debug(&'a dyn std::fmt::Debug),
    #[doc(hidden)]
    EntityArgument(&'static crate::Attribute<EntityHandle>, &'a EntityHandle),
}

struct ThreadLog {
    handle: EntityHandle,
    next: Cell<Option<u64>>,
    record_relation: usize,
}

impl ThreadLog {
    fn new() -> io::Result<Self> {
        let mut object = Map::new();
        if let Some(name) = std::thread::current().name() {
            object.insert("name".into(), name.into());
        }
        Ok(Self {
            handle: EntityHandle::with_id(
                runtime()
                    .relation(schema::log_thread.__meta())
                    .map_err(io::Error::other)?,
                gettid::gettid(),
                object,
            )?,
            next: Cell::new(Some(0)),
            record_relation: runtime()
                .relation(schema::log_record.__meta())
                .map_err(io::Error::other)?,
        })
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Record<'a> {
    pub thread: &'a EntityHandle,
    pub seq: u64,
    pub record_relation: usize,
}

/// Render a pre-start diagnostic in isolation, without claiming a delivery address.
pub(crate) fn with_preview_record<T>(f: impl FnOnce(Record<'_>) -> io::Result<T>) -> io::Result<T> {
    let log = ThreadLog::new()?;
    f(Record {
        thread: &log.handle,
        seq: 0,
        record_relation: log.record_relation,
    })
}

/// All outputs on the thread share a sequence, but track accepted definitions
/// separately. A failed write can leave a gap, never a dangling reference.
pub(crate) fn with_record<T>(f: impl FnOnce(Record<'_>) -> io::Result<T>) -> io::Result<T> {
    thread_local! { static LOG: io::Result<ThreadLog> = ThreadLog::new(); }
    LOG.try_with(|log| {
        let log = log
            .as_ref()
            .map_err(|e| io::Error::new(e.kind(), e.to_string()))?;
        let seq = log
            .next
            .get()
            .ok_or_else(|| io::Error::other("thread record sequence exhausted"))?;
        let next = seq.checked_add(1);
        log.next.set(next);
        let result = f(Record {
            thread: &log.handle,
            seq,
            record_relation: log.record_relation,
        });
        // A custom sink can log through another writer while this write is in flight.
        if result.is_err() && log.next.get() == next {
            log.next.set(Some(seq));
        }
        result
    })
    .map_err(|_| io::Error::other("thread record sequence unavailable"))?
}
