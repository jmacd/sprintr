// SPDX-License-Identifier: Apache-2.0

use crate::__private::{BorrowedArgument, Unsigned, unsigned};
use crate::{EntityHandle, RelationDefinition, RelationKind, RelationType};
use std::io;
use std::marker::PhantomData;

/// Metadata shared by attribute and entity relation declarations.
#[doc(hidden)]
#[derive(Debug)]
pub struct RelationMeta {
    pub(crate) name: &'static str,
    kind: RelationKind,
    value_type: DeclaredType,
    anonymous: bool,
    pub(crate) description: &'static str,
}

#[derive(Debug)]
enum DeclaredType {
    Literal(RelationType),
    Entity(&'static RelationMeta),
}

impl RelationMeta {
    #[doc(hidden)]
    pub const fn __new(
        name: &'static str,
        kind: RelationKind,
        value_type: RelationType,
        anonymous: bool,
        description: &'static str,
    ) -> Self {
        Self {
            name,
            kind,
            value_type: DeclaredType::Literal(value_type),
            anonymous,
            description,
        }
    }

    #[doc(hidden)]
    pub const fn __entity_attribute(
        name: &'static str,
        desc: &'static str,
        target: &'static Entity<true>,
    ) -> Self {
        Self {
            name,
            kind: RelationKind::Attribute,
            value_type: DeclaredType::Entity(target.__meta()),
            anonymous: false,
            description: desc,
        }
    }

    pub(crate) fn definition(
        &self,
        recorded_name: impl FnOnce(&'static RelationMeta) -> String,
    ) -> RelationDefinition {
        RelationDefinition {
            kind: self.kind,
            value_type: match &self.value_type {
                DeclaredType::Literal(ty) => ty.clone(),
                DeclaredType::Entity(target) => RelationType::Entity(recorded_name(target)),
            },
            desc: self.description.into(),
            anonymous: self.anonymous,
        }
    }

    pub(crate) fn kind_name(&self) -> &'static str {
        match self.kind {
            RelationKind::Attribute => "attribute",
            RelationKind::Entity => "entity",
        }
    }
}

/// A typed attribute declared with [`crate::attributes!`].
///
/// Positional arguments support `str`, `u64`, and `entity(relation)`; `[u64]` describes stack metadata.
pub struct Attribute<T: ?Sized> {
    meta: RelationMeta,
    label: &'static [u8],
    reference: &'static [u8],
    value_type: PhantomData<T>,
}

impl<T: ?Sized> Attribute<T> {
    #[doc(hidden)]
    pub const fn __new(meta: RelationMeta, label: &'static [u8], reference: &'static [u8]) -> Self {
        Self {
            meta,
            label,
            reference,
            value_type: PhantomData,
        }
    }

    /// The declared name; a collision may record it under another name.
    pub const fn name(&self) -> &'static str {
        self.meta.name
    }

    pub const fn description(&self) -> &'static str {
        self.meta.description
    }

    #[doc(hidden)]
    pub const fn __meta(&'static self) -> &'static RelationMeta {
        &self.meta
    }

    #[doc(hidden)]
    pub const fn __label(&self) -> &'static [u8] {
        self.label
    }

    #[doc(hidden)]
    pub const fn __reference(&self) -> &'static [u8] {
        self.reference
    }
}

impl Attribute<str> {
    #[doc(hidden)]
    pub fn __value<'a, V: AsRef<str> + ?Sized>(&self, value: &'a V) -> BorrowedArgument<'a> {
        BorrowedArgument::String(value.as_ref())
    }

    #[doc(hidden)]
    pub fn __string_value<'a, V: AsRef<str> + ?Sized>(&self, value: &'a V) -> BorrowedArgument<'a> {
        self.__value(value)
    }
}

impl Attribute<u64> {
    #[doc(hidden)]
    pub fn __value<V: Unsigned + ?Sized>(&self, value: &V) -> BorrowedArgument<'static> {
        BorrowedArgument::Unsigned(unsigned(value))
    }

    #[doc(hidden)]
    pub fn __hex_value<V: Unsigned + ?Sized>(&self, value: &V) -> BorrowedArgument<'static> {
        self.__value(value)
    }
}

impl Attribute<EntityHandle> {
    #[doc(hidden)]
    pub fn __value<'a>(&'static self, handle: &'a EntityHandle) -> BorrowedArgument<'a> {
        BorrowedArgument::EntityArgument(self, handle)
    }

    pub(crate) fn check<'a>(&self, handle: &'a EntityHandle) -> io::Result<&'a EntityHandle> {
        let DeclaredType::Entity(target) = self.meta.value_type else {
            return Err(io::Error::other("expected an entity-valued attribute"));
        };
        let runtime = crate::runtime::runtime();
        let expected = runtime.relation(target).map_err(io::Error::other)?;
        if expected != handle.relation {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "attribute {:?} expects an entity of {:?}, got {:?}",
                    self.name(),
                    runtime.definitions[expected].name,
                    handle.key
                ),
            ));
        }
        Ok(handle)
    }
}

/// An entity relation declared with [`crate::entities!`].
pub struct Entity<const ANONYMOUS: bool = false> {
    meta: RelationMeta,
}

impl<const ANONYMOUS: bool> Entity<ANONYMOUS> {
    #[doc(hidden)]
    pub const fn __new(meta: RelationMeta) -> Self {
        Self { meta }
    }

    /// The declared name; a collision may record it under another name.
    pub const fn name(&self) -> &'static str {
        self.meta.name
    }

    pub const fn description(&self) -> &'static str {
        self.meta.description
    }

    #[doc(hidden)]
    pub const fn __meta(&'static self) -> &'static RelationMeta {
        &self.meta
    }
}

/// A statically named entity, such as `log.level:INFO`.
pub struct Named {
    relation: &'static Entity,
    name: &'static str,
    description: &'static str,
    fragment: &'static [u8],
}

impl Named {
    #[doc(hidden)]
    pub const fn __new(
        relation: &'static Entity,
        name: &'static str,
        description: &'static str,
        fragment: &'static [u8],
    ) -> Self {
        Self {
            relation,
            name,
            description,
            fragment,
        }
    }

    pub const fn relation(&self) -> &'static Entity {
        self.relation
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }

    pub const fn description(&self) -> &'static str {
        self.description
    }

    /// The template `attrs` member, such as `"log.level":"INFO"`.
    #[doc(hidden)]
    pub const fn __fragment(&self) -> &'static [u8] {
        self.fragment
    }
}

/// One link-time registration emitted by the declarators.
#[doc(hidden)]
pub enum Declaration {
    Relation(&'static RelationMeta),
    Named(&'static Named),
}
