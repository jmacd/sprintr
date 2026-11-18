## Objective

A Rust logging library that:

- Compiles message formatting strings using source material
- Uses structured logging, including nested structure
- Enables formatted log recovery
- Adds a new fundamental value-reference
  - refer to values in the same log stream,
  - or refer to a qualified log record in another stream
- Includes file:line and a limited number of stack frames
- Supports declaring entities that are named or anonymous
- Supports declaring attributes with descriptions and types
- Encodes definitions for callsite, stackframe, entity, attribute once
  per log stream; uses references to refer to definitions
- Reference syntax uses record identifiers and body-relative JSON Pointer
  fragments
- Encodes directly to OTLP LogRecord bytes (minimizes allocations)

## Details

### Message formats

Users of slogr can log `format!`-like expressions in Rust while
retaining structured formatting. Message format placeholders may
contain expression text or attribute names, and they can be
overridden. Arguments are positional.

| Statement                              | Message format             | Note                    |
|----------------------------------------|----------------------------|-------------------------|
| `info!("hello!")`                       | "hello!"                   | no variables            |
| `info!("this {abc:s} and that {def:s}")`  | "this {abc} and that {def}" | inline variable names   |
| `info!("sum {:f} quot {:f}", x+y, x/y)`   | "sum {x+y} quot {x/y}"      | `:f` => `Into<f64>` arg  |
| `info!("hello {}", attr = a)`           | "hello {attr}"             | `attr` must be declared |
| `info!("I am {name:s}")`                | "I am {name}"              | `:s` => `AsRef<str>`     |
| `info!("see {:?}", variable)`           | "see {variable}"           | `:?` => `Debug`         |
| `info!("tabular {:04x} {:08x}", l, r)`   | "tabular {l:04x} {r:08x}"   | `:x` => hex             |

Generally, we aim to stay close to https://doc.rust-lang.org/std/fmt/
while adding more type format designations, generally following the C
printf letters, such as `:s` matching C's `%s`. The message format omits
formatting arguments that do not alter format; for example, `:?` does
not explain how to format, while `:#` does.

Note that every argument must have a known type inside the macro
expansion, so that we can statically examine all logging callsites and
build known-type data structures in advance. In the future, we expect
to explore constructing Arrow record batches directly instead of OTLP
bytes, and we want to know column types in advance; however, as
discussed below, we will have to add columns to the OTAP logs
representation, and for now we are embedding our new fields within
the structure of the LogRecord body field.

## Attributes

Attributes, generally those which are used by more than one callsite,
are declared explicitly using OpenTelemetry Weaver semantic definition
files. In this case, the attributes specify the structured argument
type that will be encoded for the attribute, along with the attribute
name and description. A Weaver annotation supports the additional type
information which semantic conventions do not convey. When no annotation
is present, the type is presumed to be string.

```
groups:
  - id: registry.slogr_example
    type: attribute_group
    brief: Attributes used by the slogr example process.
    attributes:
      - ref: my_str   # default type is string
      - ref: my_int
        annotations:
          slogr:
            type: u64
```

Generated code allows users to import attribute definitions:

```rust
use super::attribute::{my_str, my_int};

info!("blah blah {} {}", my_str = a, my_int = b);
```

Compilation will fail if the argument cannot be interpreted as
the indicated type.

## Entities

Entities are declared similarly to attributes; however, whereas
attributes define properties of log events, entities define properties
of values within log events. Weaver code generation will be used to
declare these in a similar manner to attributes, as they have the same
representation and structure (KeyValue). Entities are passed by handle
to the log API; entities are constructed using the entity definition
and a structured value; the value is evaluated and encoded once,
eagerly, when the entity handle is constructed.

Note that this concept of entity is not directly related to the
experimental OpenTelemetry Entities specification.

We have two categories of entity, named and anonymous. Callers
construct named entities by providing a globally distinct string-value
identifier. Semantically, a named entity is used to correlate the
entity across process boundaries; they should be used when there
is a natural, globally given name for the entity.

Anonymous entities are a convenience for entities that are locally
unique without a natural namespace; these entities are given unique
integer identifiers, which may be randomly or sequentially assigned by
the library, and which should not be presumed equal unless assigned
within the same process. (Process identity is discussed below.)

```
groups:
  - id: registry.slogr_example
    type: attribute_group
    brief: Attributes used by the slogr example process.
    attributes:
      - ref: my_thing
        annotations:
          slogr:
            entity: named  # implies type: string
```

Callers will allocate long-lived entity-value objects:

```rust
use super::entity::{my_thing};

let handle = my_thing.new("thing_name", <expression>);

info!("starting {}", handle);
```

The `<expression>` above is a placeholder for a structured value, as
provided by the `as_struct()` method (or something similarly idiomatic). In
this example, the entity key-value `my_thing:thing_name` is defined by
the expression and logged once; future uses of the `handle` are logged
as an entity reference.

For anonymous entities, simply remove the name and let the library
fill one in:

```rust
use super::entity::{my_session};

let handle = my_session.new(<expression>);

info!("new session {}", handle);
```

Logging entity values has the effect of placing a reference to the
entity value in the log.

## Arguments

Message format strings must contain exactly as many placeholders as
there are positional arguments at the callsite. When >= 1 argument is
present, the structured values are placed under the `args` section of
the body, as a list.
    
```
{
  ...
  "args": [
    <first_arg>,
    <second_arg>,
    ...
  ]
}
```

## References

When an entity handle is used, its value is recorded in the log record
body for future reference. The entity
handle retains a unique identifier given to the associated log record
where it was defined in the log. Log record unique identifiers consist
of unique identifier(s) of the execution environment plus a sequence
number. For example, a unique processor identifier and a unique thread
name plus a sequence number are suitable; a task-local or synchronized
logger may use different uniqueness coordinates. How the process
self-identifies will be written in the initial log record.

Entity handles store the record identifier where their definition
appears. The entry definition will include the entity key:value
expression and associated structured value, like `{
"my_thing:thing_name": <expression-value> }`. Note that an
`<expression-value>` may itself contain entity references.

References will be logged at least once. This requires entity handles
to contain inner mutability via a `Cell` or otherwise idiomatic Rust,
so as to be able to update the log record identifier. (If a shared
entity handle is required, an `Arc<Mutex<_>>` or `OnceLock` may be
required; however, this is not initially required.)

An inline entity reference will appear using the "$ref" key (like JSON
Schema). For example, when logging a reference using an entity handle,
the value will appear as a reference like `{ "$ref":
"<record_id>#/defs/my_thing:thing_name" }`, in addition to being
defined at least once.

The `refs` section of the body is an array of single-entry maps of
the form `{ "<entity-name>": "<fragment>" }`. Each entry
associates an entity name with the location of its value in the containing
record's `args` or `defs`, including nested values. Since these values
are always reference fragments, no `$ref` wrapper is needed. For example, an anonymous entity
whose value is stored directly in `args[0]` is referenced as follows:

```json
{
  "args": [4],
  "refs": [
    { "my_session:12345678": "#/args/0" }
  ]
}
```

References are also used to convey an association between attributes
and positional arguments. For example, when a log statement uses a
`my_attr = entity_handle` argument, two associations are in effect:

1. One reference to `my_thing:thing_name` appears as a `$ref` value in
   `args[0]`, referring to the entity value, e.g., as shown above in
   the defs section of an earlier log record.
2. One reference appears in the record attributes, e.g. `{ "my_attr":
   { "$ref": "#/args/0" } }`, referring to the entity value reference
   in the same log record.

## Definitions

The `defs` section of the record body is a map from entity names to
their values, which may contain entity references. Definitions are
logged at least once; however, note that
entities are synchronized independently of log record emission, so the
entity may be committed to a log record that is recorded out of order with
other records. An entity's first reference may appear before an
entity's definition. Readers should tolerate late-arriving
definitions.

Entities are (conditionally) encoded after the logs sampler interface's
`should_sample` method returns true.

```
{
  ...
  "defs": {
    "my_thing:thing_name": <expression-value>
  }
}
```

An entity value that itself contains other entities will encode refs
to other defs. Entity handles are erased from structured values, and
entries in `refs` associate their entity names with locations in the
definition. For example, this `defs` contains a `b` key logged using
an anonymous entity handle with value `4`:

```json
{
  "defs": {
    "my_thing:thing_name": { "a": 3, "b": 4 }
  },
  "refs": [
    { "b_attr:12345678": "#/defs/my_thing:thing_name/b" }
  ]
}
```

Note that the order of encoding is non-deterministic: the same data
could equivalently be encoded with `b_attr:12345678` defined as `4`,
with a ref embedded in `my_thing:thing_name`.

## JSON Structure

To stay compatible with OTLP log structure, the `body` field contains
the fields described above, including `defs`, `refs`, `seq`, `args`,
and `msg`. Log attributes, scope attributes, and resource attributes
are encoded in the appropriate level of OTLP structure.

## Paths and graph reconstruction

Reference fragments use the URI fragment representation of
[JSON Pointer (RFC 6901)](https://www.rfc-editor.org/rfc/rfc6901#section-6).
The pointer root is the decoded `LogRecord.body`, not the whole OTLP
record or its protobuf-JSON representation. This applies even to
references stored in record attributes. For example, `#/args/0`
identifies the first argument, and `#/defs/my_thing:thing_name/b`
identifies a nested field in an entity definition.

Within each path component, `~` is escaped as `~0` and `/` as `~1`.
The resulting pointer is then URI-fragment encoded, using UTF-8 and
percent-encoding where required. For example, the entity name
`file:/tmp/a` is addressed by `#/defs/file:~1tmp~1a`.

A qualified reference has the form `<record_id>#/<pointer-path>`:
the record identifier selects a record in the global collection, and
the pointer selects a node within its body. The URI representation of
record identifiers remains to be specified. A fragment-only reference
implicitly selects the containing record. Entries in `refs` use local
fragments targeting `args` or `defs`; inline `$ref` values may also
identify targets in other records.

Log records collectively describe a directed graph. Loading resolves
reference targets into graph edges, preserving shared nodes and cycles
rather than recursively expanding referenced values. JSON Pointers
are interpreted against the encoded record structure before graph
linking changes its representation; following a pointer does not
implicitly traverse an inline `$ref`.

Targets may arrive out of order. Loaders register target nodes and
connect pending edges as records become available. An unavailable
target remains explicitly unresolved, rather than becoming a null
value. Cycles are valid graph structure, not resolution errors.

## Startup

## Context

## Application

## Additional context

https://gist.github.com/jmacd/17a6f2885d92d7864ba76680a8ee367a
