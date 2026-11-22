// SPDX-License-Identifier: Apache-2.0

use crate::__private::{BorrowedArgument, Unsigned, unsigned};
use crate::{RelationDefinition, RelationKind, RelationType};
use std::marker::PhantomData;

/// Metadata shared by attribute and entity relation declarations.
#[doc(hidden)]
#[derive(Debug)]
pub struct RelationMeta {
    pub(crate) name: &'static str,
    kind: RelationKind,
    value_type: RelationType,
    anonymous: bool,
    pub(crate) description: &'static str,
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
            value_type,
            anonymous,
            description,
        }
    }

    pub(crate) fn definition(&self) -> RelationDefinition {
        RelationDefinition {
            kind: self.kind,
            value_type: self.value_type,
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
/// `T` is `str` or `u64`. Logging macros check argument types against `T`.
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
