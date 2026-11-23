// SPDX-License-Identifier: Apache-2.0

use crate::entity::{EntityHandle, Record};
use crate::runtime::runtime;
use crate::statement::{EncoderCache, NestedTemplate};
use crate::stream::{Accepted, Pending};
use crate::{Value, json_error, schema};
use serde::{Serialize, Serializer};
use std::borrow::Cow;
use std::fmt::{self, Write as _};
use std::io::{self, Write};

/// A borrowed argument that supplies its own structured representation.
pub trait StructuredValue {
    /// Write exactly one value. Pass children to the writer so it can track
    /// their record paths and detect back-edges before invoking their encoders.
    fn encode(&self, out: &mut dyn ValueWriter) -> io::Result<()>;

    #[doc(hidden)]
    fn __borrow(&self) -> Value<'_>
    where
        Self: Sized,
    {
        Value::Structured(self)
    }

    #[doc(hidden)]
    fn __type_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}

/// Writes one node of the enclosing event, without creating another event.
pub trait ValueWriter {
    /// Encode the handle itself: this argument slot holds a single `$ref`.
    fn entity(&mut self, entity: &EntityHandle) -> io::Result<()>;
    /// A named struct; field names and values are stored literally.
    fn object(&mut self, name: &str, fields: &[(&str, Value<'_>)]) -> io::Result<()>;
    /// A plain positional sequence.
    fn array(&mut self, values: &[Value<'_>]) -> io::Result<()>;
    #[doc(hidden)]
    fn __formatted(
        &mut self,
        format: &'static NestedTemplate,
        args: &[Value<'_>],
    ) -> io::Result<()>;
}

pub fn formatted_value(f: impl Fn(&mut dyn ValueWriter) -> io::Result<()>) -> impl StructuredValue {
    struct Formatted<F>(F);
    impl<F: Fn(&mut dyn ValueWriter) -> io::Result<()>> StructuredValue for Formatted<F> {
        fn encode(&self, out: &mut dyn ValueWriter) -> io::Result<()> {
            self.0(out)
        }
    }
    Formatted(f)
}

pub trait StructuredArgument {
    fn __sprintr_argument(&self) -> Value<'_>;
}
impl<T: StructuredValue> StructuredArgument for T {
    fn __sprintr_argument(&self) -> Value<'_> {
        self.__borrow()
    }
}
impl StructuredArgument for dyn StructuredValue + '_ {
    fn __sprintr_argument(&self) -> Value<'_> {
        Value::Structured(self)
    }
}
impl StructuredValue for EntityHandle {
    fn encode(&self, out: &mut dyn ValueWriter) -> io::Result<()> {
        out.entity(self)
    }
    fn __borrow(&self) -> Value<'_> {
        Value::Entity(self)
    }
}

// Within a structured positional array, bare unsigned fields render in decimal.
pub trait ScalarArgument {
    fn __sprintr_argument(&self) -> Value<'_>;
}
impl<T: crate::__private::Unsigned> ScalarArgument for T {
    fn __sprintr_argument(&self) -> Value<'_> {
        Value::Unsigned(self.as_u64())
    }
}
impl ScalarArgument for str {
    fn __sprintr_argument(&self) -> Value<'_> {
        Value::String(self)
    }
}
impl ScalarArgument for String {
    fn __sprintr_argument(&self) -> Value<'_> {
        Value::String(self)
    }
}

struct Active {
    address: usize,
    ty: &'static str,
    path_len: usize,
}

#[derive(Default)]
pub(crate) struct Scratch {
    pub refs: Vec<u8>,
    path: String,
    label: String,
    active: Vec<Active>,
}

impl Scratch {
    pub fn clear(&mut self) {
        self.refs.clear();
        self.path.clear();
        self.active.clear();
    }
}

fn invalid(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

struct DebugText<'a>(&'a dyn fmt::Debug);
impl Serialize for DebugText<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&format_args!("{:?}", self.0))
    }
}

pub(crate) struct Encoder<'a, 'event> {
    buf: &'a mut dyn Write,
    scratch: &'a mut Scratch,
    encoders: &'a mut EncoderCache,
    state: &'a Accepted,
    pending: &'a mut Pending<'event>,
    record: Record<'a>,
}

impl<'a, 'event> Encoder<'a, 'event> {
    pub fn new(
        buf: &'a mut dyn Write,
        scratch: &'a mut Scratch,
        encoders: &'a mut EncoderCache,
        state: &'a Accepted,
        pending: &'a mut Pending<'event>,
        record: Record<'a>,
    ) -> Self {
        Self {
            buf,
            scratch,
            encoders,
            state,
            pending,
            record,
        }
    }

    pub fn args(&mut self, args: &[Value<'event>]) -> io::Result<()> {
        for (i, &arg) in args.iter().enumerate() {
            if i != 0 {
                self.buf.write_all(b",")?;
            }
            let entity = match arg {
                Value::Entity(e) => Some(e),
                Value::EntityArgument(attr, e) => Some(attr.check(e)?),
                _ => None,
            };
            if let Some(e) = entity {
                let seq = self
                    .pending
                    .entity(self.state, self.record, e, || Cow::Borrowed(&e.member));
                self.entity_ref(e, seq)?;
                continue;
            }
            if matches!(arg, Value::Structured(_)) {
                self.scratch.path.clear();
                write!(self.scratch.path, "#/args/{i}").expect("write to string");
            }
            self.value(arg)?;
        }
        Ok(())
    }

    fn value(&mut self, arg: Value<'_>) -> io::Result<()> {
        match arg {
            Value::String(s) => serde_json::to_writer(&mut self.buf, s).map_err(json_error),
            Value::Unsigned(n) => serde_json::to_writer(&mut self.buf, &n).map_err(json_error),
            Value::Debug(d) => {
                serde_json::to_writer(&mut self.buf, &DebugText(d)).map_err(json_error)
            }
            Value::EntityArgument(attr, e) => self.value(Value::Entity(attr.check(e)?)),
            Value::Entity(e) => {
                let seq = self
                    .pending
                    .entity(self.state, self.record, e, || Cow::Owned(e.member.to_vec()));
                self.entity_ref(e, seq)
            }
            Value::Structured(value) => {
                let address = std::ptr::from_ref(value).cast::<()>().addr();
                let ty = value.__type_name();
                // Zero-sized values need not have distinct addresses. Bound
                // traversal even when a formatter creates fresh values recursively.
                if std::mem::size_of_val(value) != 0
                    && let Some(parent) = self
                        .scratch
                        .active
                        .iter()
                        .rev()
                        .find(|a| a.address == address && a.ty == ty)
                {
                    return write!(
                        self.buf,
                        "{{\"$ref\":\"{}\"}}",
                        &self.scratch.path[..parent.path_len]
                    );
                }
                if self.scratch.active.len() == 32 {
                    return Err(invalid("structured argument nesting exceeds 32"));
                }
                self.scratch.active.push(Active {
                    address,
                    ty,
                    path_len: self.scratch.path.len(),
                });
                let mut slot = Slot {
                    encoder: self,
                    written: None,
                };
                let result = value.encode(&mut slot);
                let complete = slot.written == Some(true);
                self.scratch.active.pop();
                result?;
                if complete {
                    Ok(())
                } else {
                    Err(invalid(
                        "formatter must write exactly one complete structured value",
                    ))
                }
            }
        }
    }

    fn annotation(&mut self, key: &str) -> io::Result<()> {
        annotation(&mut self.scratch.refs, &self.scratch.path, key)
    }

    fn entity_ref(&mut self, e: &EntityHandle, seq: u64) -> io::Result<()> {
        write!(self.buf, "{{\"$ref\":\"{seq}{}\"}}", e.fragment)
    }

    fn elements(&mut self, args: &[Value<'_>], bindings: &[(usize, usize)]) -> io::Result<()> {
        self.buf.write_all(b"[")?;
        let parent = self.scratch.path.len();
        let mut bindings = bindings.iter().peekable();
        for (i, &arg) in args.iter().enumerate() {
            if i != 0 {
                self.buf.write_all(b",")?;
            }
            write!(self.scratch.path, "/{i}").expect("write to string");
            while let Some(&&(_, relation)) = bindings.peek().filter(|b| b.0 == i) {
                self.pending.schema(self.state, relation);
                self.annotation(&runtime().definitions[relation].name)?;
                bindings.next();
            }
            self.value(arg)?;
            self.scratch.path.truncate(parent);
        }
        self.buf.write_all(b"]")
    }
}

fn annotation(refs: &mut Vec<u8>, path: &str, key: &str) -> io::Result<()> {
    if !refs.is_empty() {
        refs.push(b',');
    }
    refs.push(b'{');
    serde_json::to_writer(&mut *refs, key).map_err(json_error)?;
    refs.extend_from_slice(b":\"");
    // The path is made exclusively from RFC 6901 URI-fragment tokens.
    refs.extend_from_slice(path.as_bytes());
    refs.extend_from_slice(b"\"}");
    Ok(())
}

struct Slot<'a, 'b, 'event> {
    encoder: &'a mut Encoder<'b, 'event>,
    written: Option<bool>,
}
impl Slot<'_, '_, '_> {
    fn one(&mut self, f: impl FnOnce(&mut Encoder<'_, '_>) -> io::Result<()>) -> io::Result<()> {
        if self.written.replace(false).is_some() {
            return Err(invalid("formatter wrote more than one value"));
        }
        f(self.encoder)?;
        self.written = Some(true);
        Ok(())
    }
}
impl ValueWriter for Slot<'_, '_, '_> {
    fn entity(&mut self, entity: &EntityHandle) -> io::Result<()> {
        self.one(|e| e.value(Value::Entity(entity)))
    }

    fn object(&mut self, name: &str, fields: &[(&str, Value<'_>)]) -> io::Result<()> {
        self.one(|e| {
            if name.is_empty() {
                return Err(invalid("struct name must not be empty"));
            }
            e.pending.schema(
                e.state,
                runtime()
                    .relation(schema::struct_name.__meta())
                    .map_err(io::Error::other)?,
            );
            let scratch = &mut e.scratch;
            scratch.label.clear();
            scratch.label.push_str("struct.name:");
            scratch.label.push_str(name);
            annotation(&mut scratch.refs, &scratch.path, &scratch.label)?;
            e.buf.write_all(b"{")?;
            let parent = e.scratch.path.len();
            for (i, &(key, value)) in fields.iter().enumerate() {
                if key == "$ref" || fields[..i].iter().any(|(k, _)| *k == key) {
                    return Err(invalid(
                        "structured field name is duplicate or reserved ($ref)",
                    ));
                }
                if i != 0 {
                    e.buf.write_all(b",")?;
                }
                serde_json::to_writer(&mut e.buf, key).map_err(json_error)?;
                e.buf.write_all(b":")?;
                e.scratch.path.push('/');
                sprintr_message::push_pointer_token(&mut e.scratch.path, key);
                e.value(value)?;
                e.scratch.path.truncate(parent);
            }
            e.buf.write_all(b"}")
        })
    }

    fn array(&mut self, values: &[Value<'_>]) -> io::Result<()> {
        self.one(|e| e.elements(values, &[]))
    }

    fn __formatted(
        &mut self,
        format: &'static NestedTemplate,
        args: &[Value<'_>],
    ) -> io::Result<()> {
        self.one(|e| {
            let nested = format.resolve()?;
            e.pending.schema(
                e.state,
                runtime()
                    .relation(schema::struct_encode.__meta())
                    .map_err(io::Error::other)?,
            );
            let ty = e.scratch.active.last().expect("active structured value").ty;
            let encoder = e.encoders.resolve(format, ty, &nested.msg)?;
            e.pending.encoder(e.state, e.record.thread.id, encoder);
            annotation(&mut e.scratch.refs, &e.scratch.path, &encoder.key)?;
            e.elements(args, &nested.bindings)
        })
    }
}
