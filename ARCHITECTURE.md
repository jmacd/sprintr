## Objective

sprintr (Structured PRINT in Rust) is a structured printing library that:

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
  per log stream by default; uses references to refer to definitions
- Reference syntax uses record identifiers and body-relative JSON Pointer
  fragments
- Builds a self-describing, self-bootstrapping graph of log records
- Supports graph navigation and cross-process tracing through references
- Encodes directly into reusable or stack-backed storage, using JSON
  initially and binary formats when useful

## Performance goals

**Performance is a primary outcome of this experiment.** Proving that
the API, schema, and graph work is not sufficient if logging imposes
avoidable allocations or repeats expensive encoding. Production-quality
producer performance must shape the implementation from the beginning,
not be postponed until a later rewrite or a switch to binary encoding.

The ordinary producer path should borrow arguments, use compile-time
format metadata, and encode directly into reusable or stack-backed
storage. The target
is zero steady-state library allocations for records that fit available
storage, excluding work performed by caller expressions and output
sinks. It must not require constructing an owned value tree per event.
Parsing formats, copying argument strings into intermediate objects,
and allocating argument collections do not belong on this path.

Entity construction pays for capturing and encoding an immutable
snapshot once. Subsequent uses emit references, rather than evaluating
or encoding the entity value again. Re-emitting a definition should
reuse its encoded chunks, with deferred references and framing assembled
as needed. Reference bookkeeping must not introduce per-use allocations
on the steady-state path.

The original direct OTLP implementation provides the performance model:
borrowed values, static metadata, direct writes, reusable storage, and
explicit handling of buffer limits. The unused tracing/OTLP runtime has
been removed; the prototype retains adapted heap/stack storage and
rollback mechanisms rather than maintaining a second logging system.
JSON and binary encoders should share these principles. The choice of
wire format is secondary to avoiding repeated work and allocations.

Performance evaluation must distinguish one-time construction and
buffer growth from repeated logging. It must measure allocation counts
and bytes, throughput, latency, contention, and retained memory. As
entities are introduced, repeated references must be measured separately
from their initial encoding. Allocation regressions should be guarded
explicitly; zero allocations alone do not establish production readiness.

Owned decoded records and graph construction belong on the reader side
and need not follow the producer's allocation budget. When a producer
buffer is bounded, overflow must be reported without emitting malformed
JSON or breaking the correspondence between formats and arguments.

## Details

### Message formats

Users of sprintr can log `format!`-like expressions in Rust while
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

Each callsite is represented by an anonymous `log.statement` entity
containing its message format, static argument bindings, and level.
Source file and line information can be added there as well.
Its value is pre-encoded with the source code.
Log events reference the statement and supply argument values, allowing
the recorded format to be reused when recovering formatted messages.

## Relations

The schema is self-describing, built on a base entity, `log.relation`.
A relation definition describes a name, whether it denotes an attribute
or an entity, its description, and its value type. Readers presume an
understanding of the base relation; the remaining concepts are described
through relation definitions in the graph.

Each process uses one shared namespace and consistent definitions
across all logging scopes. Names are used directly, without namespace
qualification; dots in names such as `log.level` do not introduce
separate namespaces. Scopes neither shadow names nor change their
meaning. Definitions may appear in multiple records or scopes, provided
their values agree.

Named definitions are immutable within a process. A conflicting
definition cannot replace an existing name, and readers reject
conflicting definitions of one name. Because a logging library must not
prevent its process from starting, the producer instead records a later,
different declaration under a distinct name and reports the collision
explicitly in the log stream; see [Startup](#startup). Agreement
means semantic equality, not identical encoded bytes: serialization
order or references to different records containing the same entity
must not alone cause a conflict. The comparison mechanism remains to
be specified; the prototype compares decoded definitions structurally.

Value types include references to entities described by a particular
relation, not only primitive values. For example, `log.level` describes
named severity entities `INFO`, `WARN`, and `ERROR`, each carrying a
description. Statement metadata `{"log.level":"INFO"}` refers to the
named entity `log.level:INFO`. The entity type is itself described by
`log.relation:log.level`; the level name is not an untyped string value.

## Attributes

The prototype declares attributes statically, as shown below, so their
metadata and the statements using them can be pre-encoded at build time.
For shared definitions, the intended schema-driven path uses OpenTelemetry
Weaver semantic definition files. In this case, the attributes specify the structured argument
type that will be encoded for the attribute, along with the attribute
name and description. A Weaver annotation supports the additional type
information which semantic conventions do not convey. When no annotation
is present, the type is presumed to be string.

```
groups:
  - id: registry.sprintr_example
    type: attribute_group
    brief: Attributes used by the sprintr example process.
    attributes:
      - ref: my_str   # default type is string
      - ref: my_int
        annotations:
          sprintr:
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
attributes label values, entities give values a shared identity.
Weaver code generation will be used to
declare these in a similar manner to attributes, as they have the same
representation and structure (KeyValue). Entities are passed by handle
to the log API; entities are constructed using the entity definition
and a structured value. Construction evaluates the value exactly once
and eagerly encodes it into a reusable intermediate representation.
The entity is a construction-time snapshot; logging does not
re-evaluate the expression.

The intermediate representation retains encoded chunks for ordinary
values, deferred slots for nested entities, and the entity dependencies
needed to fill those slots. It need not be a complete OTLP byte string:
a nested entity's definition location may not yet be known. Final
assembly reuses the encoded chunks and fills deferred slots with inline
entity values or references, according to the selected definition
locations. Reference strings and enclosing protobuf lengths are
finalized during assembly, rather than patched into fixed-size slots.

Note that this concept of entity is not directly related to the
experimental OpenTelemetry Entities specification.

We have two categories of entity, named and anonymous. Callers
construct named entities by providing a globally distinct string-value
identifier. Semantically, a named entity is used to correlate the
entity across process boundaries; they should be used when there
is a natural, globally given name for the entity.

Repeated definitions of a named entity within a process must have the
same value, as described under Relations; they are not updates to a
mutable named value.

Anonymous entities are a convenience for entities that are locally
unique without a natural namespace; these entities are given unique
integer identifiers, which may be randomly or sequentially assigned by
the library, and which should not be presumed equal unless assigned
within the same process. (Process identity is discussed below.)

Each log record is itself an anonymous entity described by `log.record`.
Records participate in the same graph as other entities and can be
located through that relation.

```
groups:
  - id: registry.sprintr_example
    type: attribute_group
    brief: Attributes used by the sprintr example process.
    attributes:
      - ref: my_thing
        annotations:
          sprintr:
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

let handle = my_session.new(session_data)?;

info!("new session {}", handle)?;
info!("resumed {}", handle)?;
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

The handle holds an immutable snapshot. On first use, the writer puts its
value in `defs["<relation>:<identity>"]` and remembers the location **for that
output and thread**, only after the write succeeds. Reusing the handle in a
different output or thread introduces a new local definition of the same
identity. The handle need not be mutated or locked.

Each emitted record carries `thread` (the OS-assigned `log.thread` ID) and
`seq` (the `log.record` ID within that thread, starting at zero). The first record on
each output from that thread defines `log.thread:<id>`; a name, if available,
is descriptive data, not identity. `log.record` names the containing record
itself, so it needs no second copy in `defs`. Sequence numbers are shared
across outputs on a thread. They increase but need not be contiguous within
any one output. The writer does not disambiguate recycled OS thread IDs.
Repeated addresses are errors, not a second incarnation of the same record.

An inline entity reference will appear using the "$ref" key (like JSON
Schema). For example, when logging a reference using an entity handle,
the value will appear as a reference like `{ "$ref":
"<record_id>#/defs/my_thing:thing_name" }`, in addition to being
defined at least once. In the prototype, `<record_id>` is the decimal sequence
on the referencing record's thread. For instance, `2#/defs/session:1` selects
the `session:1` definition **in record 2**, not simply the most recent object
with that name. A standalone snapshot uses `#/defs/session:1` in its own body.

The `refs` section of the body is an array of single-entry maps.
Ordinary references have the form `{ "<attribute-or-entity-name>": "<fragment>" }`. Each entry
associates a name with the location of its value in the containing
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
2. One association appears in `refs`, e.g. `{ "my_attr": "#/args/0" }`,
   referring to the entity value reference
   in the same log record.

The same `refs` association labels a primitive argument, such as
`{ "account.name": "#/args/0" }` for a string. The list preserves multiple
associations using the same name; it is not a map keyed by attribute name.
Event `attrs` holds non-positional associations, including the reference to
`log.statement`. Static severity belongs to the statement's `attrs`.
Within statement templates, positional associations use explicit parameter
bindings instead: `{ "account.name": { "$arg": 0 } }`. These bindings are
instantiated for each event, not resolved against the template definition.

## Definitions

The `defs` section of the record body is a map from entity names to
their values, which may contain entity references. Definitions are
logged at least once. Entities are synchronized independently of log
record emission. Although each scope emits records in ascending
sequence order, readers may observe references before their definitions,
including when combining records from different scopes. Readers should
tolerate late-arriving definitions.

Definition locations are selected and final OTLP bytes are assembled
from the intermediate representations only after the logs sampler
interface's `should_sample` method returns true. A rejected log record
does not assign definition locations or mark entities as defined;
eager construction-time encoding is independent of this decision.

The initial design assumes reliable delivery of emitted records; it
does not require a durable-delivery acknowledgment protocol for
definitions. Record loss is nevertheless tolerated: references to a
lost definition may remain explicitly unresolved without preventing
the rest of the graph from being loaded.

Entity definitions may be repeated periodically to limit the impact
of loss on later references that target the repeated definitions.
Repetition in a new record does not by itself repair references to a
missing earlier record. The repetition policy and recovery of those
earlier references are left for future work.

```
{
  ...
  "defs": {
    "my_thing:thing_name": <expression-value>
  }
}
```

An entity value that itself contains other entities will encode refs
to other defs. Entity handles are absent from the wire representation,
but their identities and dependencies are retained in the intermediate
representation until assembly. Entries in `refs` associate entity
names with locations in the definition. For example, the anonymous
entity's value `4` is encoded inline under `b` in the parent definition;
the `refs` entry identifies that existing node as `b_attr:12345678`:

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
the fields described above, including `attrs`, `defs`, `refs`, `args`,
and `msg`. In JSON Lines, the top-level `seq`, `stmt`, and `thread` fields abbreviate
`attrs["log.record"]`, `attrs["log.statement"]`, and `attrs["log.thread"]`. They have the same
meaning as the expanded associations and must not also appear in `attrs`
under their full names. A snapshot without a delivery address omits `seq` and `thread`.
Log attributes, scope attributes, and resource attributes
are encoded in the appropriate level of OTLP structure.

## Paths and graph reconstruction

Reference fragments use the URI fragment representation of
[JSON Pointer (RFC 6901)](https://www.rfc-editor.org/rfc/rfc6901#section-6).
The pointer root is the logical record body: the JSON object itself in
the standalone prototype, or the decoded `LogRecord.body` when using
OTLP, not the whole OTLP record or its protobuf-JSON representation.
For example, `#/args/0`
identifies the first argument, and `#/defs/my_thing:thing_name/b`
identifies a nested field in an entity definition.

Within each path component, `~` is escaped as `~0` and `/` as `~1`.
The resulting pointer is then URI-fragment encoded, using UTF-8 and
percent-encoding where required. For example, the entity name
`file:/tmp/a` is addressed by `#/defs/file:~1tmp~1a`.

A record-relative reference has the form `<seq>#/<pointer-path>`:
the sequence selects a record on the current thread, and the pointer
selects a node within its body. A fragment-only reference
implicitly selects the containing record. Ordinary entries in `refs` use local
fragments targeting `args` or `defs`; inline `$ref` values may also
identify targets in other records. A statement's `{ "$arg": n }` binding
is a distinct template parameter, not a JSON Pointer. It associates a name
with argument `n` of each event using the template. The resulting graph
edges connect to event argument values, never to nonexistent template arguments.

For exchange between processes, the intended **graph** address of a record is
`#/<global_attr>/<global_value>/log.thread/<thread_id>/log.record/<seq>`.
The first pair must uniquely identify the process execution; a PID alone is
not enough. An RPC peer could exchange the address of a start record and
link to the corresponding end records using ordinary entities and attributes.
This global index and span linkage are not implemented. The thread and record
components are the same ones used for local references.

Log records collectively describe a directed graph. Loading resolves
reference targets into graph edges, preserving shared nodes and cycles
rather than recursively expanding referenced values. JSON Pointers
are interpreted against the encoded record structure before graph
linking changes its representation; following a pointer does not
implicitly traverse an inline `$ref`.

Cycles are natural in this graph. Serialization can choose where to
stop nesting values and encode an edge as a `$ref` instead. This breaks
recursive expansion in the serialized representation, not the cycle
in the graph; loading reconnects the edge to its target.

Targets may arrive out of order. Loaders register target nodes and
connect pending edges as records become available. An unavailable
target remains explicitly unresolved, rather than becoming a null
value. Cycles are valid graph structure, not resolution errors.

The reconstructed graph also provides a process-wide relation lookup.
Within that process's graph view, `/log.relation/<name>/desc` selects
a relation's description; `kind` and type information describe its
meaning. The name is escaped as a JSON Pointer component. This is a
lookup in the reconstructed graph, not the wire layout of a record's
`defs` map. JSON Pointer provides navigation; the loader establishes
the process context and relation lookup. The full relation schema and
its wire representation remain to be specified.

## Startup

The initial log record of each output announces the process and the
logging library, and contains the definitions of the relations and
entities it references, bootstrapping the graph from the presumed
understanding of `log.relation`. Later records reuse those definitions.
Only `log.relation` is built into the wire format. `log.statement`,
`log.level`, and the announcement's attributes are declared like any
other relation, although readers recognize the built-in meaning of
`log.statement` when formatting.

Attributes and entity relations are declared in code with readable
declarators, and each declaration's documentation is its description.
Declarations are collected at link time and resolved once, when logging
first starts. The library's own schema resolves first, then other
declarations in link order. Equal declarations share a name. A later,
different declaration of an existing name is recorded under a
content-derived name, such as `account.name_1a2b3c4d`, and statements
using it refer to that name. Each rename is announced in a `WARN` record
immediately after the startup record, so collisions are visible without
preventing the process from starting. Later definitions extend the same
process-wide namespace; repeated definitions must agree.

Common definitions can be generated from external schema files,
including the Weaver definitions described above. A more restrictive
future model would require common named definitions to come from those
files and allow only anonymous entities to be created at runtime.
Library-assigned unique identities avoid named-definition conflicts
for those anonymous entities. Build-time validation can catch schema
conflicts earlier, while process-wide consistency checks can catch
incompatible definitions supplied by different dependencies.

## Context

Each local logging scope defines a sequence of log records, emitted in
ascending sequence order. The OTel instrumentation scope carries the
defining attributes, and its associated records are grouped under that
scope. Different scopes share the process's definitions; they do not
create separate namespaces. There is no implied total order across
scopes.

Threads are entities described by the `log.thread` relation. A thread
instance records its details and is distinct from the definition of its
entity type. Callsites are anonymous `log.statement` entities; stack
frames are entities, and callstacks are composed of references to frame
entities. Each concept is described through a relation rooted in
`log.relation`. Placement of this context in OTel resources is deferred.

## Application

### Graph navigation

The in-log reference is a fundamental part of the design, not merely
a way to avoid repeating values. Once records are loaded into the
graph, entity identities and attribute-value associations connect
their occurrences to the records containing them. Navigation can
follow those relationships in either direction to locate every
appearance of an entity or attribute value in the loaded records.

Path expressions over the graph can select matching records by
attribute or entity and traverse the relationships between them.
The `log.record` relation provides access to the records themselves.
This graph-query capability is distinct from JSON Pointer addressing
of individual wire targets; its expression language and indexing
mechanisms are not specified here.

### Tracing through log references

A guiding idea is that a trace span can be represented by two related
log events: a start and an end. The start event is addressable by a
global log reference, and the end event can refer back to it. Context
propagation carries that reference, connecting activity through the
same graph used for ordinary logging.

For an RPC, the client records its start event and sends a global
reference to that event with the request. The server records the
propagated reference in its own log. In its response, the server
returns a reference to one of its own log records, which the client
records in its log, for example in the end event.

Both sides therefore record references into the other side's log.
When loaded together, their records are part of the same global graph
and can be navigated across the RPC boundary. The exact propagation
format and mapping to OpenTelemetry tracing are left open; the design
principle is to express the relationship using ordinary log references.

## First milestone: message format and arguments

This section records the initial slice before statement deduplication.
The current wire format is described in the statement-template extension
below; the initial per-record `msg` format is no longer emitted or accepted.

Start with JSON Lines recording and plain-text playback of a message
format and its positional arguments. Each line contains only `msg`
and `args`, representing the logical record body without OTLP framing.
There are no attributes, entities (including callsites and `log.record`),
definitions, references, or schema bootstrap in this milestone. The
message format is repeated in each record.

The standalone `sprintr-prototype` crate implements this first slice
without Tokio or `tracing`. It is an API and text-format prototype, not
a production logger. For example:

```rust
use sprintr_prototype::info;

let name = "Ada";
let value = 42u32;

info!("hello {:s}, value {:04x}", name, value)?;
```

The equivalent implicit-capture form is
`info!("hello {name:s}, value {value:04x}")?`.
Both return output errors to the caller. The prototype defaults to
JSON Lines on stdout; an optional `logger:` argument selects a
caller-owned writer. `info!` encodes borrowed arguments directly into
reusable storage, using a statically encoded format prefix. Heap and
fixed stack buffers reuse the original implementation's storage and
rollback approach. Bounded buffers reject a whole record on overflow,
rather than truncating JSON or dropping arguments.

Owned record construction is available separately through `message!`.
This convenience path may allocate and is not required for logging.

The recorded JSON line is:

```json
{"msg":"hello {name}, value {value:04x}","args":["Ada",42]}
```

Placeholder names are labels derived from the source expressions, not
attribute declarations. Placeholders consume `args` in occurrence order.
The string designation `:s` is omitted from the stored format, while
`:04x` is retained because it controls rendering. The integer stays a
JSON number rather than becoming the preformatted string `"002a"`.

Reading that line produces:

```text
hello Ada, value 002a
```

The milestone is complete when a writer records this structure and a
reader reconstructs the plain-text message from the JSON line alone,
applying the stored formatting operations to the argument values. This
small example exercises string insertion and zero-padded hexadecimal
integer formatting; full formatting-language coverage is deferred.
The prototype supports `AsRef<str>` strings and `u8`, `u16`, `u32`, and
`u64` integers, including alternate hexadecimal prefixes and escaped
braces. Unsupported formats and argument types fail at compilation;
malformed recorded input fails during playback. The obsolete `otel_*`
runtime and its examples are no longer part of the workspace.

The following extension adds self-describing attribute definitions.
Generic entity references and binary encoding remain later goals.
A web-based log browser is also a
later goal: it will preserve a conjunction of entity/attribute pairs
while pivoting between chronological and frequency-based views of the
matching records. Those features are outside this first milestone.

## Static attributes and statement-template extension

The next slice declares an attribute statically and pre-encodes a
statement template with its canonical name. Declaration emits no record:

```rust
use sprintr_prototype::{attributes, info};

attributes! {
    /// Account name
    account_name: str = "account.name";
}

let value = 42u32;
for name in ["Ada", "Grace"] {
    info!("hello {}, value {value:04x}", account_name = name)?;
}
```

The handle specifies the argument type, allowing a bare `{}`. Attributed
placeholders use canonical names prefixed with `@`, not Rust variable names.
Each callsite defines an anonymous `log.statement` entity whose value
contains only static `attrs`, argument bindings in `refs`, and `msg`:

```json
{"attrs":{"log.level":"INFO"},"refs":[{"account.name":{"$arg":0}}],"msg":"hello {@account.name}, value {value:04x}"}
```

`log.relation:log.statement` describes anonymous object entities;
`log.relation:account.name` describes the named string attribute. A template
contains neither dynamic `args` nor nested `defs`. Its `attrs` contains a
static reference to the named `log.level:INFO` entity. The startup record
already defined `log.statement`, `log.level`, and `log.level:INFO`, so the
first event bootstraps only the remaining definitions. The next event carries
its own address but reuses the statement:

```json
{"seq":1,"stmt":2,"thread":88888,"args":["Ada",42],"defs":{"log.relation:account.name":{"kind":"attribute","type":"string","desc":"Account name"},"log.statement:2":{"attrs":{"log.level":"INFO"},"refs":[{"account.name":{"$arg":0}}],"msg":"hello {@account.name}, value {value:04x}"}}}
{"seq":2,"stmt":2,"thread":88888,"args":["Grace",42]}
```

`"stmt":2` is a typed reference to `defs["log.statement:2"]`.
The nonzero integer identity is assigned on first use and is process-local,
not a globally qualified reference; the startup announcement used `1`. Each
callsite has its own identity, even when its template is equal to another's.
After the announcement, rendering produces:

```text
hello Ada, value 002a
log.level = INFO, account.name = Ada
hello Grace, value 002a
log.level = INFO, account.name = Grace
```

Definitions are immutable and process-wide. Equal declarations may appear
in multiple modules; a different declaration of an existing name is renamed
and announced at startup rather than failing. Static attribute and statement
values are encoded during
compilation, not lazily encoded on the first event. Runtime work assigns
statement identities and tracks which definitions each output has accepted.
Stdout keeps this bookkeeping per thread, allowing equal repeated definitions
across threads. First-use bookkeeping may allocate, even with stack-backed
storage; subsequent recording must preserve the zero-allocation target and
does not lock the schema registry.

The reader compiles templates once into formatting operations and argument
bindings. It applies them to each event without copying an expanded record.
Missing or conflicting definitions, invalid bindings, and type mismatches
are errors. Owned `message!` snapshots include their definitions and render
independently. `message!` captures an unlevelled snapshot; `info!`, `warn!`,
and `error!` bake `INFO`, `WARN`, or `ERROR` into their statement templates.
The named values carry the descriptions "Informational event", "Warning
condition", and "Error condition". Each output emits a level definition only
when first needed, and reuses it across statements. Application-defined
thread-local attribute injection is a later step.
The current extension supports string and `u64` attributes, statically named
entities, anonymous runtime objects, anonymous statements, `$arg` bindings,
and record-relative references to whole object definitions. Runtime named
entities, general graph linking, filtering, and graph navigation remain future work.

## Additional context

https://gist.github.com/jmacd/17a6f2885d92d7864ba76680a8ee367a
