// SPDX-License-Identifier: Apache-2.0

use crate::{
    __private::{accepted_address, dispatch},
    Attribute, EntityHandle, EventArgs, EventKind, RecordAddress, SpanRef, Value,
    declare::EntityArgument,
    entity::next_uid,
    statement::Statement,
    structured::StructuredArgument,
};
use sprintr_message::{EXCEPTION, EXCEPTION_CAUSE, ExceptionTarget};
use std::{
    error::Error,
    fmt,
    io::{self, Write},
    num::NonZeroU64,
    panic::Location,
    rc::Rc,
};

/// Application error payload, owned locally. Clones refer to the same failure.
///
/// ```compile_fail
/// fn transfer<T: Send>() {}
/// transfer::<sprintr_prototype::StructuredError>();
/// ```
#[derive(Clone)]
pub struct StructuredError(Rc<Captured>);

struct Captured {
    site: &'static ErrorSite,
    fields: Box<[OwnedField]>,
    stack: Box<[u64]>,
    cause: Option<Cause>,
    target: Option<ExceptionTarget>,
}

impl StructuredError {
    /// Promote an accepted occurrence to a `Send + Sync` reference. This
    /// does not capture another stack, retry delivery, or copy the fields.
    pub fn reference(&self) -> Option<ExceptionRef> {
        let target = self.0.target?;
        target.origin.map(|_| ExceptionRef(target))
    }

    pub fn class(&self) -> &'static str {
        self.0.site.class
    }
    pub fn location(&self) -> &'static Location<'static> {
        self.0.site.location
    }
    pub fn stack(&self) -> &[u64] {
        &self.0.stack
    }
}

impl fmt::Display for StructuredError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (self.0.site.display)(&self.0.fields, f)
    }
}
impl fmt::Debug for StructuredError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct(self.class())
            .field("message", &format_args!("{self}"))
            .field("location", self.location())
            .finish()
    }
}
impl Error for StructuredError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(match self.0.cause.as_ref()? {
            Cause::Local(e) => e,
            Cause::Recorded(r) => r,
        })
    }
}

/// Transferable address of the original definition, independent of the local payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExceptionRef(ExceptionTarget);

impl ExceptionRef {
    pub fn origin(self) -> RecordAddress {
        self.0.origin.expect("promoted occurrence")
    }
    pub fn id(self) -> NonZeroU64 {
        self.0.id
    }
}
impl fmt::Display for ExceptionRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl Error for ExceptionRef {}

#[doc(hidden)]
pub enum Cause {
    Local(StructuredError),
    Recorded(ExceptionRef),
}
impl From<&StructuredError> for Cause {
    fn from(e: &StructuredError) -> Self {
        Self::Local(e.clone())
    }
}
impl From<&ExceptionRef> for Cause {
    fn from(r: &ExceptionRef) -> Self {
        Self::Recorded(*r)
    }
}
impl Cause {
    fn target(&self) -> io::Result<ExceptionTarget> {
        match self {
            Self::Local(e) => {
                e.0.target
                    .ok_or_else(|| io::Error::other("cause UID unavailable"))
            }
            Self::Recorded(r) => Ok(r.0),
        }
    }
}

#[doc(hidden)]
pub struct ErrorSite {
    statement: &'static Statement,
    class: &'static str,
    location: &'static Location<'static>,
    display: fn(&[OwnedField], &mut fmt::Formatter<'_>) -> fmt::Result,
}
impl ErrorSite {
    pub const fn new(
        statement: &'static Statement,
        class: &'static str,
        location: &'static Location<'static>,
        display: fn(&[OwnedField], &mut fmt::Formatter<'_>) -> fmt::Result,
    ) -> Self {
        Self {
            statement,
            class,
            location,
            display,
        }
    }
}

type Role = &'static Attribute<EntityHandle>;

#[doc(hidden)]
pub enum OwnedField {
    Text(Box<str>),
    Unsigned(u64),
    Entity(Option<Role>, EntityHandle),
    Span(SpanRef),
    Exception(Option<Role>, Option<ExceptionTarget>),
    Invalid(&'static str),
}

impl OwnedField {
    pub fn capture(arg: Value<'_>) -> Self {
        match arg {
            Value::String(s) => Self::Text(s.into()),
            Value::Unsigned(n) => Self::Unsigned(n),
            Value::Debug(value) => {
                let mut s = String::new();
                if fmt::write(&mut s, format_args!("{value:?}")).is_err() {
                    return Self::Invalid("Debug formatter failed");
                }
                Self::Text(s.into_boxed_str())
            }
            Value::Entity(e) => Self::Entity(None, e.clone()),
            Value::EntityArgument(role, e) => Self::Entity(Some(role), e.clone()),
            Value::Span(s) => Self::Span(s.clone()),
            Value::Exception(role, t) => Self::Exception(role, t.copied()),
            Value::Structured(_) => {
                Self::Invalid("borrowed structured formatter cannot be retained in an exception")
            }
        }
    }
    fn borrow(&self) -> Value<'_> {
        match self {
            Self::Text(s) => Value::String(s),
            Self::Unsigned(n) => Value::Unsigned(*n),
            Self::Entity(Some(role), e) => Value::EntityArgument(role, e),
            Self::Entity(None, e) => Value::Entity(e),
            Self::Span(s) => Value::Span(s),
            Self::Exception(role, t) => Value::Exception(*role, t.as_ref()),
            Self::Invalid(_) => unreachable!("checked before delivery"),
        }
    }
}
impl fmt::Display for OwnedField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(s) => f.write_str(s),
            Self::Unsigned(n) => fmt::Display::fmt(n, f),
            Self::Entity(_, e) => f.write_str(e.encoded_body()),
            Self::Span(s) => write!(f, "{}", s.start().span_start_ref()),
            Self::Exception(_, Some(t)) => write!(f, "{}", t.diagnostic()),
            Self::Exception(_, None) => f.write_str("<exception UID unavailable>"),
            Self::Invalid(why) => write!(f, "<unavailable: {why}>"),
        }
    }
}
impl fmt::LowerHex for OwnedField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsigned(n) => fmt::LowerHex::fmt(n, f),
            _ => Err(fmt::Error),
        }
    }
}

impl EntityArgument for StructuredError {
    fn with_role(&self, role: Role) -> Value<'_> {
        Value::Exception(Some(role), self.0.target.as_ref())
    }
}
impl EntityArgument for ExceptionRef {
    fn with_role(&self, role: Role) -> Value<'_> {
        Value::Exception(Some(role), Some(&self.0))
    }
}
impl StructuredArgument for StructuredError {
    fn __sprintr_argument(&self) -> Value<'_> {
        Value::Exception(None, self.0.target.as_ref())
    }
}
impl StructuredArgument for ExceptionRef {
    fn __sprintr_argument(&self) -> Value<'_> {
        Value::Exception(None, Some(&self.0))
    }
}

/// Bare exception fields must outlive the invocation through an owned handle.
#[doc(hidden)]
pub trait RetainedArgument: StructuredArgument {
    fn __sprintr_retained_argument(&self) -> Value<'_> {
        self.__sprintr_argument()
    }
}
impl RetainedArgument for EntityHandle {}
impl RetainedArgument for SpanRef {}
impl RetainedArgument for StructuredError {}
impl RetainedArgument for ExceptionRef {}

#[derive(Clone, Copy)]
pub(crate) struct Occurrence {
    id: NonZeroU64,
    pub cause: Option<ExceptionTarget>,
}
impl Occurrence {
    pub(crate) fn write_definition(self, out: &mut impl Write) -> io::Result<()> {
        write!(out, "\"{EXCEPTION}:{}\":{{", self.id)?;
        if let Some(t) = self.cause {
            write!(out, "\"{EXCEPTION_CAUSE}\":{{\"$ref\":\"{t}\"}}")?;
        }
        out.write_all(b"}")
    }
}

#[doc(hidden)]
pub fn capture_exception<const N: usize>(
    site: &'static ErrorSite,
    stack: io::Result<&[u64]>,
    fields: [OwnedField; N],
    cause: Option<Cause>,
) -> StructuredError {
    let uid = next_uid().map(|n| NonZeroU64::new(n).expect("positive entity UID"));
    let mut target = uid
        .as_ref()
        .ok()
        .map(|&id| ExceptionTarget { id, origin: None });
    let stack_pcs: Box<[u64]> = stack.as_ref().map_or(&[][..], |s| *s).into();
    let origin = accepted_address(uid.and_then(|id| {
        if let Some(msg) = fields.iter().find_map(|f| {
            if let OwnedField::Invalid(msg) = f {
                Some(*msg)
            } else {
                None
            }
        }) {
            return Err(io::Error::other(msg));
        }
        let occurrence = Occurrence {
            id,
            cause: cause.as_ref().map(Cause::target).transpose()?,
        };
        dispatch(
            site.statement,
            Ok(stack?),
            EventArgs {
                values: &fields.each_ref().map(OwnedField::borrow),
                kind: EventKind::Exception(occurrence),
            },
        )
    }));
    if let Some(t) = &mut target {
        t.origin = origin;
    }
    StructuredError(Rc::new(Captured {
        site,
        fields: Box::new(fields),
        stack: stack_pcs,
        cause,
        target,
    }))
}
