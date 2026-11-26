// SPDX-License-Identifier: Apache-2.0

use crate::runtime::runtime;
use crate::{
    Definition, Entity, IdentityValue, Named, RECORD, RelationDefinition, RelationType, SOURCE,
    STATEMENT, SpanRef, THREAD, schema,
};
use serde::Serialize;
use serde_json::{Map, Value};
use std::cell::Cell;
use std::io;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A thread-local identity for an immutable application object. Clones retain
/// the same identity and encoded payload without copying the object.
#[derive(Clone)]
pub struct EntityHandle {
    pub(crate) id: u64,
    pub(crate) relation: usize,
    pub(crate) payload: Rc<EntityPayload>,
}

pub(crate) struct EntityPayload {
    pub(crate) key: String,
    pub(crate) fragment: String,
    pub(crate) member: Box<[u8]>,
    body_offset: usize,
    identity: Rc<EntityIdentity>,
}

/// One concrete level of an identity. A handle owns the typed keys of its
/// entire scope chain, independently of TLS and of where it is first logged.
#[derive(Debug)]
pub struct EntityIdentity {
    relation: usize,
    values: Box<[IdentityValue]>,
    within: Option<Rc<Self>>,
}

impl EntityIdentity {
    pub fn entity_type(&self) -> &'static str {
        &runtime().definitions[self.relation].name
    }
    pub fn declaration(&self) -> &'static RelationDefinition {
        let Definition::Relation(schema) = &runtime().definitions[self.relation].definition else {
            unreachable!("relation index")
        };
        schema
    }
    /// Ordered as in `declaration().id_keys`. A static name or assigned UID occupies one slot.
    pub fn values(&self) -> &[IdentityValue] {
        &self.values
    }
    pub fn within(&self) -> Option<&Self> {
        self.within.as_deref()
    }

    fn key(&self) -> io::Result<String> {
        let mut parts = Vec::new();
        let mut next = Some(self);
        while let Some(part) = next {
            parts.push(part.values());
            next = part.within();
        }
        parts.reverse();
        Ok(format!(
            "{}:{}",
            self.entity_type(),
            serde_json::to_string(&parts).map_err(crate::json_error)?
        ))
    }
}

pub(crate) fn next_uid() -> io::Result<u64> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map_err(|_| io::Error::other("entity identity space exhausted"))
}

/// Local key values in declaration order. Scalars and pairs can be passed
/// directly; arrays of `IdentityValue` support arbitrary compound IDs.
pub struct IdentityValues(Box<[IdentityValue]>);
macro_rules! scalar_keys {
    ($($t:ty),*) => { $(impl From<$t> for IdentityValues {
        fn from(value: $t) -> Self { Self(Box::new([IdentityValue::from(value)])) }
    })* };
}
scalar_keys!(&str, String, u8, u16, u32, u64, IdentityValue);
impl<A: Into<IdentityValue>, B: Into<IdentityValue>> From<(A, B)> for IdentityValues {
    fn from((a, b): (A, B)) -> Self {
        Self(Box::new([a.into(), b.into()]))
    }
}
impl<T: Into<IdentityValue>, const N: usize> From<[T; N]> for IdentityValues {
    fn from(values: [T; N]) -> Self {
        Self(Box::new(values.map(Into::into)))
    }
}
impl From<Vec<IdentityValue>> for IdentityValues {
    fn from(values: Vec<IdentityValue>) -> Self {
        Self(values.into_boxed_slice())
    }
}

fn object(value: impl Serialize) -> io::Result<Map<String, Value>> {
    match serde_json::to_value(value).map_err(crate::json_error)? {
        Value::Object(object) => Ok(object),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "entity payload must be an object",
        )),
    }
}

impl Entity<true> {
    /// Give a serializable object a new process-local identity. No record is written yet.
    #[allow(clippy::new_ret_no_self, clippy::wrong_self_convention)]
    pub fn new(&'static self, value: impl Serialize) -> io::Result<EntityHandle> {
        let runtime = runtime();
        let invalid = |msg| io::Error::new(io::ErrorKind::InvalidInput, msg);
        let relation = runtime.relation(self.__meta()).map_err(invalid)?;
        let name = &runtime.definitions[relation].name;
        if matches!(
            name.as_str(),
            STATEMENT | RECORD | THREAD | SOURCE | sprintr_message::EXCEPTION
        ) {
            return Err(invalid(format!("{name} values are created by the logger")));
        }
        EntityHandle::with_id(relation, next_uid()?, object(value)?)
    }
}

impl Entity<false, false> {
    /// Construct a globally named object using this declaration's ID attributes.
    pub fn named(
        &'static self,
        id: impl Into<IdentityValues>,
        value: impl Serialize,
    ) -> io::Result<EntityHandle> {
        named_handle(self.__meta(), None, id, value)
    }
}

/// Binds a scoped declaration to a concrete container, before supplying its local ID.
pub struct Within<'a> {
    meta: &'static crate::declare::RelationMeta,
    container: IdentityScopeRef<'a>,
}

/// A concrete identity scope, chosen independently of the current thread context.
pub enum IdentityScopeRef<'a> {
    Entity(&'a EntityHandle),
    Named(&'static Named),
    Span(&'a SpanRef),
}

impl<'a> From<&'a EntityHandle> for IdentityScopeRef<'a> {
    fn from(handle: &'a EntityHandle) -> Self {
        Self::Entity(handle)
    }
}
impl From<&'static Named> for IdentityScopeRef<'_> {
    fn from(named: &'static Named) -> Self {
        Self::Named(named)
    }
}
impl<'a> From<&'a SpanRef> for IdentityScopeRef<'a> {
    fn from(span: &'a SpanRef) -> Self {
        Self::Span(span)
    }
}

impl Entity<false, true> {
    pub fn within<'a>(&'static self, container: impl Into<IdentityScopeRef<'a>>) -> Within<'a> {
        Within {
            meta: self.__meta(),
            container: container.into(),
        }
    }
}

impl Within<'_> {
    pub fn named(
        self,
        id: impl Into<IdentityValues>,
        value: impl Serialize,
    ) -> io::Result<EntityHandle> {
        named_handle(self.meta, Some(self.container), id, value)
    }
}

fn named_handle(
    meta: &'static crate::declare::RelationMeta,
    container: Option<IdentityScopeRef<'_>>,
    id: impl Into<IdentityValues>,
    value: impl Serialize,
) -> io::Result<EntityHandle> {
    let invalid = |msg| io::Error::new(io::ErrorKind::InvalidInput, msg);
    let rt = runtime();
    let relation = rt.relation(meta).map_err(io::Error::other)?;
    let declared = &rt.definitions[relation];
    let layout = meta
        .identity
        .as_ref()
        .ok_or_else(|| invalid("expected named entity".into()))?;
    if layout.keys.is_empty()
        || matches!(
            declared.name.as_str(),
            sprintr_message::SPAN_START
                | sprintr_message::TRACE_START
                | sprintr_message::SPAN_PARENT
        )
    {
        return Err(invalid(format!(
            "{} does not accept application-defined named values",
            declared.name
        )));
    }
    let within = match container {
        None => None,
        Some(parent) => {
            let identity = match parent {
                IdentityScopeRef::Entity(h) => h.payload.identity.clone(),
                IdentityScopeRef::Named(n) => {
                    let relation = rt
                        .relation(n.relation().__meta())
                        .map_err(io::Error::other)?;
                    let index = rt.named(n).map_err(io::Error::other)?;
                    Rc::new(EntityIdentity {
                        relation,
                        values: Box::new([IdentityValue::String(
                            rt.definitions[index].name.clone(),
                        )]),
                        within: None,
                    })
                }
                IdentityScopeRef::Span(span) => {
                    let trace = EntityIdentity {
                        relation: rt
                            .relation(schema::trace_start.__meta())
                            .map_err(io::Error::other)?,
                        values: Box::new([format!("{:032x}", span.trace_id()).into()]),
                        within: None,
                    };
                    Rc::new(EntityIdentity {
                        relation: rt
                            .relation(schema::span_start.__meta())
                            .map_err(io::Error::other)?,
                        values: Box::new([format!("{:016x}", span.span_id()).into()]),
                        within: Some(Rc::new(trace)),
                    })
                }
            };
            let expected = rt
                .relation(layout.within.expect("scoped declaration"))
                .map_err(io::Error::other)?;
            if expected != identity.relation {
                return Err(invalid(format!(
                    "{} is scoped by {}, not {}",
                    declared.name,
                    rt.definitions[expected].name,
                    identity.entity_type()
                )));
            }
            Some(identity)
        }
    };
    let values = id.into().0;
    if values.len() != layout.keys.len() {
        return Err(invalid(format!(
            "{} requires {} identifying values",
            declared.name,
            layout.keys.len()
        )));
    }
    for (key, value) in layout.keys.iter().zip(values.iter()) {
        let schema = &rt.definitions[rt.relation(key).map_err(io::Error::other)?];
        let Definition::Relation(def) = &schema.definition else {
            unreachable!("attribute declaration")
        };
        if !matches!(
            (&def.value_type, value),
            (RelationType::String, IdentityValue::String(_))
                | (RelationType::U64, IdentityValue::Unsigned(_))
        ) {
            return Err(invalid(format!(
                "invalid identifying value for {} ({:?})",
                schema.name, def.value_type
            )));
        }
    }
    let identity = EntityIdentity {
        relation,
        within,
        values,
    };
    let key = identity.key()?;
    EntityHandle::encode(next_uid()?, identity, key, object(value)?)
}

impl EntityHandle {
    pub(crate) fn encoded_body(&self) -> &str {
        std::str::from_utf8(&self.payload.member[self.payload.body_offset..])
            .expect("serialized JSON")
    }

    pub fn identity(&self) -> &EntityIdentity {
        &self.payload.identity
    }

    pub(crate) fn is_named(&self) -> bool {
        !self.identity().declaration().anonymous
    }

    fn with_id(relation: usize, id: u64, object: Map<String, Value>) -> io::Result<Self> {
        let key = format!("{}:{id}", runtime().definitions[relation].name);
        let identity = EntityIdentity {
            relation,
            values: Box::new([IdentityValue::Unsigned(id)]),
            within: None,
        };
        Self::encode(id, identity, key, object)
    }

    fn encode(
        id: u64,
        identity: EntityIdentity,
        key: String,
        object: Map<String, Value>,
    ) -> io::Result<Self> {
        let mut member = serde_json::to_vec(&key).map_err(crate::json_error)?;
        member.push(b':');
        let body_offset = member.len();
        serde_json::to_writer(&mut member, &object).map_err(crate::json_error)?;
        Ok(Self {
            id,
            relation: identity.relation,
            payload: Rc::new(EntityPayload {
                identity: Rc::new(identity),
                fragment: sprintr_message::definition_fragment(&key),
                key,
                member: member.into(),
                body_offset,
            }),
        })
    }
}

#[derive(Clone, Copy)]
pub enum BorrowedArgument<'a> {
    String(&'a str),
    Unsigned(u64),
    Entity(&'a EntityHandle),
    Span(&'a crate::SpanRef),
    #[doc(hidden)]
    Exception(
        Option<&'static crate::Attribute<EntityHandle>>,
        Option<&'a sprintr_message::ExceptionTarget>,
    ),
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

fn with_next<T>(f: impl FnOnce(&ThreadLog, Record<'_>) -> io::Result<T>) -> io::Result<T> {
    thread_local! { static LOG: io::Result<ThreadLog> = ThreadLog::new(); }
    LOG.try_with(|log| {
        let log = log
            .as_ref()
            .map_err(|e| io::Error::new(e.kind(), e.to_string()))?;
        let seq = log
            .next
            .get()
            .ok_or_else(|| io::Error::other("thread record sequence exhausted"))?;
        f(
            log,
            Record {
                thread: &log.handle,
                seq,
                record_relation: log.record_relation,
            },
        )
    })
    .map_err(|_| io::Error::other("thread record sequence unavailable"))?
}

/// Render in isolation at the next possible address without reserving or publishing it.
pub(crate) fn with_preview_record<T>(f: impl FnOnce(Record<'_>) -> io::Result<T>) -> io::Result<T> {
    with_next(|_, record| f(record))
}

/// All outputs on the thread share a sequence, but track accepted definitions
/// separately. A failed write can leave a gap, never a dangling reference.
pub(crate) fn with_record<T>(f: impl FnOnce(Record<'_>) -> io::Result<T>) -> io::Result<T> {
    with_next(|log, record| {
        let next = record.seq.checked_add(1);
        log.next.set(next);
        let result = f(record);
        // A custom sink can log through another writer while this write is in flight.
        if result.is_err() && log.next.get() == next {
            log.next.set(Some(record.seq));
        }
        result
    })
}
