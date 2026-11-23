// SPDX-License-Identifier: Apache-2.0

use crate::runtime::runtime;
use crate::{Attribute, EntityHandle, Named, json_error};
use std::io;

/// A context association can refer to an application handle or a declared named entity.
pub enum ContextEntity<'a> {
    Anonymous(&'a EntityHandle),
    Named(&'static Named),
}

impl<'a> From<&'a EntityHandle> for ContextEntity<'a> {
    fn from(handle: &'a EntityHandle) -> Self {
        Self::Anonymous(handle)
    }
}

impl<'a> From<&'static Named> for ContextEntity<'a> {
    fn from(value: &'static Named) -> Self {
        Self::Named(value)
    }
}

/// A validated, pre-encoded association, reusable across synchronous scopes.
pub struct EntityContext<'a> {
    pub(crate) attribute: usize,
    pub(crate) relation: usize,
    pub(crate) definition: ContextDefinition<'a>,
    pub(crate) member: Box<[u8]>,
}

pub(crate) enum ContextDefinition<'a> {
    Anonymous(&'a EntityHandle),
    Named(usize),
}

impl Attribute<EntityHandle> {
    /// Bind this role to an entity of its declared relation. This emits no record.
    pub fn context<'a>(
        &'static self,
        value: impl Into<ContextEntity<'a>>,
    ) -> io::Result<EntityContext<'a>> {
        let rt = runtime();
        let attribute = rt.relation(self.__meta()).map_err(io::Error::other)?;
        let named_key;
        let (relation, key, definition) = match value.into() {
            ContextEntity::Anonymous(handle) => (
                handle.relation,
                handle.key.as_str(),
                ContextDefinition::Anonymous(handle),
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
                    ContextDefinition::Named(index),
                )
            }
        };
        self.check_relation(relation)?;
        let mut member = serde_json::to_vec(&rt.definitions[attribute].name).map_err(json_error)?;
        member.extend_from_slice(b":{\"$ref\":");
        serde_json::to_writer(&mut member, key).map_err(json_error)?;
        member.push(b'}');
        Ok(EntityContext {
            attribute,
            relation,
            definition,
            member: member.into(),
        })
    }
}

scoped_tls_hkt::scoped_thread_local!(static CURRENT: for<'a> &'a [&'a EntityContext<'a>]);

impl EntityContext<'_> {
    /// Add this association for `f` on the calling thread. An inner binding of
    /// the same attribute shadows the outer one until return or unwind.
    pub fn in_scope<R>(&self, f: impl FnOnce() -> R) -> R {
        with_current(|outer| {
            if outer.is_empty() {
                return CURRENT.set(&[self], f);
            }
            let mut active = Vec::with_capacity(outer.len() + 1);
            active.extend(
                outer
                    .iter()
                    .copied()
                    .filter(|c| c.attribute != self.attribute),
            );
            active.push(self);
            CURRENT.set(&active, f)
        })
    }
}

pub(crate) fn with_current<R>(f: impl FnOnce(&[&EntityContext<'_>]) -> R) -> R {
    if CURRENT.is_set() {
        CURRENT.with(f)
    } else {
        f(&[])
    }
}
