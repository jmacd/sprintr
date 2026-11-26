// SPDX-License-Identifier: Apache-2.0

use crate::runtime::runtime;
use crate::{Attribute, EntityHandle, Named, RecordAddress, SpanRef, json_error};
use std::io;
use std::rc::Rc;

/// A context association can refer to an application handle or a declared named entity.
pub enum ContextEntity<'a> {
    Handle(&'a EntityHandle),
    Named(&'static Named),
}

impl<'a> From<&'a EntityHandle> for ContextEntity<'a> {
    fn from(handle: &'a EntityHandle) -> Self {
        Self::Handle(handle)
    }
}

impl<'a> From<&'static Named> for ContextEntity<'a> {
    fn from(value: &'static Named) -> Self {
        Self::Named(value)
    }
}

/// An owned, pre-encoded role binding. Clones share its immutable data.
#[derive(Clone)]
pub struct EntityContext(Rc<Binding>);

pub(crate) struct Binding {
    pub(crate) attribute: usize,
    pub(crate) relation: usize,
    pub(crate) definition: ContextDefinition,
    pub(crate) member: Box<[u8]>,
}

pub(crate) enum ContextDefinition {
    Handle(EntityHandle),
    Named { index: usize, value: &'static Named },
}

impl Attribute<EntityHandle> {
    /// Bind this role to an entity of its declared relation. This emits no record.
    pub fn context<'a>(
        &'static self,
        value: impl Into<ContextEntity<'a>>,
    ) -> io::Result<EntityContext> {
        let rt = runtime();
        let attribute = rt.relation(self.__meta()).map_err(io::Error::other)?;
        let named_key;
        let (relation, key, definition) = match value.into() {
            ContextEntity::Handle(handle) => (
                handle.relation,
                handle.payload.key.as_str(),
                ContextDefinition::Handle(handle.clone()),
            ),
            ContextEntity::Named(named) => {
                let relation = rt
                    .relation(named.relation().__meta())
                    .map_err(io::Error::other)?;
                let index = rt.named(named).map_err(io::Error::other)?;
                named_key = format!(
                    "{}:{}",
                    rt.definitions[relation].name, rt.definitions[index].name
                );
                (
                    relation,
                    named_key.as_str(),
                    ContextDefinition::Named {
                        index,
                        value: named,
                    },
                )
            }
        };
        self.check_relation(relation)?;
        let mut member = serde_json::to_vec(&rt.definitions[attribute].name).map_err(json_error)?;
        member.extend_from_slice(b":{\"$ref\":");
        serde_json::to_writer(&mut member, key).map_err(json_error)?;
        member.push(b'}');
        Ok(EntityContext(Rc::new(Binding {
            attribute,
            relation,
            definition,
            member: member.into(),
        })))
    }
}

impl EntityContext {
    pub(crate) fn data(&self) -> &Binding {
        &self.0
    }

    fn value(&self) -> ContextEntity<'_> {
        match &self.0.definition {
            ContextDefinition::Handle(handle) => ContextEntity::Handle(handle),
            ContextDefinition::Named { value, .. } => ContextEntity::Named(value),
        }
    }

    /// Add this association for `f` on the calling thread. An inner binding of
    /// the same attribute shadows the outer one until return or unwind.
    pub fn in_scope<R>(&self, f: impl FnOnce() -> R) -> R {
        let mut next = Context::current();
        next.insert(self.clone());
        next.in_scope(f)
    }
}

// A prepared single binding needs no additional allocation to enter a scope.
#[derive(Clone, Default)]
enum Bindings {
    #[default]
    Empty,
    One(EntityContext),
    Many(Rc<Vec<EntityContext>>),
}

/// An owned, thread-local snapshot. Editing a clone or [`Self::current`]
/// changes only that snapshot; [`Self::in_scope`] explicitly activates it.
///
/// ```compile_fail
/// let context = sprintr_prototype::Context::root();
/// std::thread::spawn(move || context.in_scope(|| {}));
/// ```
#[derive(Clone, Default)]
pub struct Context {
    bindings: Bindings,
    origin: Option<RecordAddress>,
    span: Option<SpanRef>,
}

scoped_tls_hkt::scoped_thread_local!(static CURRENT: Context);

impl Context {
    /// An empty detached bag. No record is emitted.
    pub const fn root() -> Self {
        Self {
            bindings: Bindings::Empty,
            origin: None,
            span: None,
        }
    }

    /// Origin of this context: an accepted context creation or span start.
    pub fn origin(&self) -> Option<RecordAddress> {
        self.origin
    }

    pub(crate) fn with_origin(mut self, origin: Option<RecordAddress>) -> Self {
        self.origin = origin;
        self
    }

    /// The active span's non-owning reference, if its start was recorded.
    pub fn span(&self) -> Option<&SpanRef> {
        self.span.as_ref()
    }

    pub(crate) fn take_span(&mut self) -> Option<SpanRef> {
        self.span.take()
    }

    pub(crate) fn for_span(mut self, span: Option<SpanRef>) -> Self {
        self.origin = span.as_ref().map(SpanRef::start);
        self.span = span;
        self
    }

    /// Detach a snapshot of the active bindings, or an empty root outside any scope.
    pub fn current() -> Self {
        if CURRENT.is_set() {
            CURRENT.with(Clone::clone)
        } else {
            Self::root()
        }
    }

    /// Install this entire bag on the current thread until `f` returns or unwinds.
    pub fn in_scope<R>(&self, f: impl FnOnce() -> R) -> R {
        CURRENT.set(self, f)
    }

    /// Insert a validated role binding, replacing any binding of the same role.
    pub fn insert(&mut self, binding: EntityContext) {
        let role = binding.0.attribute;
        match &mut self.bindings {
            Bindings::Empty => self.bindings = Bindings::One(binding),
            Bindings::One(old) if old.0.attribute == role => *old = binding,
            Bindings::One(old) => {
                self.bindings = Bindings::Many(Rc::new(vec![old.clone(), binding]))
            }
            Bindings::Many(bindings) => {
                let entries = Rc::make_mut(bindings);
                if let Some(old) = entries.iter_mut().find(|e| e.0.attribute == role) {
                    *old = binding;
                } else {
                    entries.push(binding);
                }
            }
        }
    }

    /// Look up the entity bound to the declared role, resolving declaration aliases.
    pub fn get(
        &self,
        role: &'static Attribute<EntityHandle>,
    ) -> io::Result<Option<ContextEntity<'_>>> {
        Ok(self.position(role)?.map(|i| self.entries()[i].value()))
    }

    /// Remove a role from this snapshot. Returns whether the role was present.
    pub fn remove(&mut self, role: &'static Attribute<EntityHandle>) -> io::Result<bool> {
        let Some(i) = self.position(role)? else {
            return Ok(false);
        };
        match &mut self.bindings {
            Bindings::Many(entries) if entries.len() == 2 => {
                self.bindings = Bindings::One(entries[1 - i].clone());
            }
            Bindings::Many(entries) => {
                Rc::make_mut(entries).remove(i);
            }
            Bindings::One(_) => self.bindings = Bindings::Empty,
            Bindings::Empty => unreachable!("found role"),
        }
        Ok(true)
    }

    fn position(&self, role: &'static Attribute<EntityHandle>) -> io::Result<Option<usize>> {
        let index = runtime()
            .relation(role.__meta())
            .map_err(io::Error::other)?;
        Ok(self
            .entries()
            .iter()
            .position(|binding| binding.0.attribute == index))
    }

    pub(crate) fn entries(&self) -> &[EntityContext] {
        match &self.bindings {
            Bindings::Empty => &[],
            Bindings::One(binding) => std::slice::from_ref(binding),
            Bindings::Many(entries) => entries,
        }
    }
}

pub(crate) fn with_current<R>(f: impl FnOnce(&Context) -> R) -> R {
    if CURRENT.is_set() {
        CURRENT.with(f)
    } else {
        f(&Context::root())
    }
}
