## Objective

sprintr (Structured PRINT in Rust) is a structured printing library that:

- Compiles message formatting strings using source material
- Uses structured logging, including nested structure
- Enables formatted log recovery
- Adds a new fundamental value-reference
  - refer to values in the same log stream,
  - or refer to a qualified log record in another stream
- Includes file:line and a limited number of stack frames
- Records each failure as an entity so wrapping errors and later logs can
  refer to the failure and its original stack
- Supports global and local entity identities with typed identifying properties,
  including SDK-generated local UIDs
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

Entity construction pays once to encode the immutable fields. The first
accepted definition replaces those bytes with its record address and pointer;
subsequent uses emit references. Reference bookkeeping must not introduce
per-use allocations on the steady-state path.

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

Each process uses one shared namespace for **relation declarations**.
Dots in names such as `log.level` do not introduce separate namespaces.
The identity scope of an **entity instance** is a separate matter: two
instances of the same declared type can have the same local ID in different
containers. Definitions of the same fully identified entity may appear in
multiple records, provided their values agree.

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

`entities!` declares a type and its identity rule. Today the prototype
constructs instances with `.new(...)`, `.named(...)`, or
`.within(&parent).named(...)`. The proposed entity form of `structured!`
would replace those constructors. In both designs, ordinary log statements
use a handle to record facts about the instance.

### Identity rule

A declaration answers two questions: **where is the ID unique**, and **who
supplies its identifying properties**? A caller-supplied ID can be global or
local to a declared container type. An *anonymous* entity has a local ID
supplied by the SDK: the declaration names the scope and the typed UID property
to generate. It uses the same construction and reference machinery. Two such
creations with identical descriptive properties still have distinct UIDs.

For example, a node called `batch` is identified within a particular pipeline
instance. A span is identified by its SpanID within a particular TraceID; its
parent span is a separate relationship. In both cases the handle retains the
typed local and enclosing identifying properties. The JSON encoder can form a
qualified key, and a direct OTAP encoder can use those values without parsing
JSON or looking up a record. This follows the distinction between identity
scope and topology in [the OTel entity proposal](https://github.com/open-telemetry/opentelemetry-specification/pull/5361)
and [the DFE entity-reporting RFC](https://github.com/open-telemetry/otel-arrow/pull/4798).

### Entity statements (proposed API)

The following illustrates the intended declaration and call syntax. The
prototype currently uses `#[named(...)]` or `#[anonymous]` declarations and
`.named(...)` or `.new(...)` constructors with a serializable object.

```rust
attributes! {
    /// Identifier of this engine incarnation
    engine_uid: str = "engine.uid";
    /// Group name within one engine
    group_id: str = "pipeline_group.id";
    /// SDK-assigned work item ID within one group
    work_uid: u64 = "work.uid";
}

entities! {
    /// Engine incarnation
    #[identity(global, id = engine_uid)]
    engine = "engine";
    /// Pipeline group
    #[identity(within = engine, id = group_id)]
    group = "pipeline_group";
    /// Work item
    #[identity(within = group, id = generated(work_uid))]
    work_item = "work_item";
}

let instance = structured!(engine, "engine {}", engine_uid = uid);
let ingest = structured!(
    group, "group {} in {} uses {policy:s}",
    group_id = "ingest",
    entity_scope = instance,
);
let work = structured!(work_item, "work for {}", entity_scope = ingest);
info!("starting {work}");
```

In `structured!(group, ...)`, `group` is a Rust path to a typed `entities!`
declaration. A qualified path or import alias selects the same declaration
and recorded type. `entity_scope` supplies the **concrete container** of the
declared type. `group_id` supplies the **identifying property** once; `policy`
is **descriptive**. For `work_item` the SDK assigns `work_uid` at construction.
There is no second name argument or separate `Serialize`/`json!` payload in
this design. The existing value-only form, `structured!("...", ...)`, describes
how to render a value; it does not assign that value an identity.

The declaration determines identity; the message determines presentation.
Changing the wording or recording a new fact about policy does not rename the
group. A formatter should also be able to omit the engine from the displayed
text while retaining it in the handle. The syntax for supplying a binding
that consumes no placeholder is still open.

### Capture and first use

**Defer publication, not evaluation.** Construction evaluates the arguments
once and retains typed identity, formatting metadata, and encoded descriptive
values. An unused handle emits no record. The first accepted event using the
handle publishes its definition in `defs`; subsequent uses can refer to that
record. A rejected write leaves the definition pending. For nested entity
arguments, retain the dependencies and encoded chunks so that only reference
addresses and enclosing lengths need to be filled in at record assembly.

The scope supplied at construction determines identity. The creating thread
or active context is provenance: if creation under worker W must be observable,
capture that association at construction or record an explicit creation event.
A later first use under worker X is an observation by X. Its record address
locates the definition; it does not make X the creator or the identity scope.

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

### Structured arguments

Building on [open-telemetry/otel-arrow#4450](https://github.com/open-telemetry/otel-arrow/pull/4450),
a Rust struct can hold entity handles and implement `StructuredValue`.
Its implementation can supply named fields directly or use `structured!` for
formatting: positional arguments, optionally bound to declared attributes,
just as in a logging statement. A positional argument can itself be another
structured value. The reader needs both the typed fields and their formatting
instructions; producing a formatted string during capture loses the fields.
Named objects carry a `struct.name:<type>` association in the outer `refs`.
Formatted arguments are positional arrays with a `struct.encode:<type>_<hash>`
association at their path. Each output emits that named encoder definition
once per thread, committing first use after a successful write. Its `msg`
is the canonical stored format, `name` the full Rust type (including generic
arguments), and `file` and `line` locate the `structured!` invocation. Replay
compiles the definition once and reuses it. `:?` captures Rust `Debug` text.

A type that represents an entity directly delegates `encode` to its handle:
its argument slot holds only a `$ref` to the definition. A type that contains
an entity can format it with `structured!("handling {}", active_session = self.handle)`.
The encoder reference targets the positional array; `active_session`'s
attribute name targets the child slot. The `$ref` in that slot identifies the
entity and its defining record. Explicit entity identity entries in `refs`
are for literal values whose identity the argument data does not convey.

The outer record owns `refs` and `defs` for the whole traversal. `args` holds
the application data; attribute and entity associations point into it rather
than wrapping each field in metadata. An inline `$ref` stands in for a value
when expanding it would close a cycle. For example, a node whose `next` is
itself can be encoded like this (record excerpt):

```json
{
  "args": [{"name": "root", "next": {"$ref": "#/args/0"}}],
  "refs": [{"struct.name:Node": "#/args/0"}]
}
```

For this to terminate, the encoder must know a node's identity and intended
location **before** descending into its children. The recursive formatting API
must expose such edges to the encoder; it cannot detect a cycle inside opaque
user code that recurses on its own. The pointer above targets the occurrence
in this record. Publishing that location to other records waits for acceptance.

## References

The intended default handle is thread-local (`Rc`). It starts with encoded
bytes and any deferred nested references. On first use, the originating thread
emits the definition. Once the output accepts the record, all local clones see
the recorded location and the handle releases the payload. A rejected write
leaves the payload available; it creates no reference target.

Sharing is explicit. Exporting an already recorded handle produces an `Arc`
containing the origin reference. For a pending handle, the preferred design
is to emit a dedicated entity-creation statement on the originating thread
before exporting. The receiving threads then need neither the bytes nor a
mutex to decide who defines the entity. The reference must identify the
originating log, thread, record, and location within that record; a bare
sequence on the receiving thread would select the wrong record. The export
API and qualified wire spelling remain to be designed.

The current JSON prototype instead defines an entity on first use per output
and thread, and retains those locations in the writer. Explicit export remains
a design goal; nested structured arguments use the current entity addresses.

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
with that name. A fragment-only `#/defs/session:1` refers to the containing record.

The `refs` section of the body is an array of single-entry maps.
Ordinary references have the form `{ "<attribute-or-entity-name>": "<fragment>" }`. Each entry
associates a name with the location of its value in the containing
record's `args` or `defs`, including nested values. Since these values
are always reference fragments, no `$ref` wrapper is needed. For example, a
literal session object in `args[0]` can be labeled with its identity:

```json
{
  "args": [{"name": "Ada"}],
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
published on first accepted use. Entities are synchronized independently of log
record emission. Although each scope emits records in ascending
sequence order, readers may observe references before their definitions,
including when combining records from different scopes. Readers should
tolerate late-arriving definitions.

Definition locations are selected and final OTLP bytes are assembled
from the intermediate representations only after the logs sampler
interface's `should_sample` method returns true. A rejected log record
does not assign definition locations or mark entities as defined;
eager construction-time encoding is independent of this decision.

A reader may cache definitions on the assumption that it can retrieve an
older record by its address: the originating log or process execution, the
thread ID, and the record sequence number. On a cache miss it can fetch that
exact record and apply the reference fragment. Delivery is imperfect: the
address identifies what to fetch, but the record may never have arrived.
In that case the graph reference remains unresolved. The current JSON Lines
reader keeps definitions in memory and reports missing targets during playback.

Future recovery options include storing definitions durably for out-of-band
retrieval through a telemetry agent, or repeating a definition when its
entity is used again after a configured interval. Later references can point
to the new record; existing references still select the original one.
Repeating a definition after its handle has released the bytes would require
retrieving the payload from storage or choosing a separate retention policy.
Storage, retrieval, and repetition policies are outside the current milestone.

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
under their full names. Each emitted record has both `seq` and `thread`.
The required `ts` field is the capture time in unsigned Unix nanoseconds.
It is not part of the record address. Text playback shows UTC to milliseconds.
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
The prototype already links spans and exceptions across threads and outputs
of one process execution. Extending those addresses with a process identity
for RPC propagation and lookup is future work.

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

Startup is explicit and fallible. Before `start()`, instrumentation writes a
warning and the event to stderr. After a startup attempt it records best-effort;
ordinary logging always returns `()` and never initiates or retries startup.
Logger selection is scoped to the current thread. `logger.with_default(f)`
installs a borrowed logger for synchronous calls under `f`, restoring the
previous selection on exit. Instrumentation has no logger argument.

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

Common relation declarations, including their identifying properties and
scope rules, can be generated from external schemas such as Weaver. The
same typed declarations can then be used to construct runtime instances.

## Context

An entity-valued attribute can be bound for a synchronous thread-local scope.
Each event in that scope carries a non-positional association such as
`"request.session":{"$ref":"session:1"}` in its own `attrs`. Here `$ref`
selects an immutable entity by its key in the stream's definitions; it does
not encode a definition location. Named and anonymous context entities follow
the same rule. `log.level` remains a static association on the statement.
Different attribute keys coexist; the innermost binding of one key wins until
scope exit. Context follows the thread across logger scopes, not spawned tasks.

`Context::current()` detaches a storable, thread-local snapshot. Editing the
snapshot leaves the installed bag and other stored copies unchanged. Activating
a `Context` replaces the whole bag, including when the replacement is an empty
root. The one-binding scope instead overlays a role on a snapshot of the current
bag. Both restore their predecessor on unwind.

Each local logging scope defines a sequence of log records, emitted in
ascending sequence order. The OTel instrumentation scope carries the
defining attributes, and its associated records are grouped under that
scope. Different scopes share the process's definitions; they do not
create separate namespaces. There is no implied total order across
scopes.

Threads are entities described by the `log.thread` relation. A thread
instance records its details and is distinct from the definition of its
entity type. Callsites are anonymous `log.statement` entities; stack
frames are anonymous `log.source` entities identified by native program
counter. Frame zero is the macro invocation's module, file, line, and column,
pre-encoded in the statement's `source` beside `msg`. Each event's `log.stack`
contains only its callers, starting with frame one, up to ten PCs by default.
A limit of zero bypasses unwinding without losing the static frame. Later events
reuse the statement template but capture their own stacks. No source objects
are emitted per PC: the reader can look up module, function, file, and line
locally after recording, or relocate the PCs with a bulk module map and
matching debug artifacts from that run. Each concept is described through a relation rooted in
`log.relation`. Placement of this context in OTel resources is deferred.

### Structured exceptions (implemented)

`exception!(LoadError::Config, "could not load {path:s}", cause = &inner)`
captures a failure and constructs the application's tuple variant. The
variant path as written names its `exception.class` on the static
`log.kind:EXCEPTION` statement; use consistently qualified paths for classes
shared between modules. An occurrence's typed fields use the statement's
ordinary positional argument bindings. Its event carries the active context,
span membership, and failure-site stack. Selecting the variant, cloning the
local error, and logging a reference each preserve that occurrence.

For example, `LoadError::Config(StructuredError)` can use `thiserror`'s
`#[error(transparent)]` to delegate `Display` and `Error::source`; the SDK
itself has no `thiserror` dependency. `cause = &inner` is a relationship, so
it consumes no message placeholder. Unlike an ordinary `info!` call, the
error must outlive the borrowed arguments: it owns its field values and
formats the message on demand. An explicit `:?` field captures Debug text
once. A later `warn!("startup failed: {problem}")` records only the reference
in that argument slot, while the warning retains its own reporting stack.

The occurrence record defines one anonymous `exception:<uid>` entity. A
leaf's object is empty; a wrapper supplies the declared `exception.cause`
entity-valued attribute:

```json
{"exception:9":{"exception.cause":{"$ref":"log.thread:42/17#/defs/exception:8"}}}
```

Here `42/17` locates the accepted *record*, and `8` identifies the *entity*
in it. The same `$ref` works as a normal log argument on any thread. If the
cause's delivery failed, `"exception:8"` identifies the unrecorded occurrence
without claiming an address. The local payload retains `Display`, `source()`
and the original raw PCs in either case. Only the reference to an accepted
record can be promoted and sent to another thread: `StructuredError` is
thread-local (`Rc`), and `.reference()` returns `Option<ExceptionRef>`. The
`Send + Sync` reference carries the UID and origin; promotion performs no I/O.

Playback validates UID/origin pairs before committing a record, including
references arriving before their defining thread's records. It distinguishes
a missing record from an unrecorded cause. Chain expansion and symbol lookup
are reader operations: each layer contributes its own message, frame zero
and stack. Exporters can derive exception message and stacktrace strings from
these fields without putting those strings on the capture path.
`crates/sprintr-prototype/examples/exception_chain.rs` demonstrates recording
and reading in separate processes: `Renderer::exception_chain` prints
`Caused by:` and preserves raw PCs when no symbol is available.

### Work plan: detachable contexts and spans

The acceptance case is a worker in span A logging
`info!("we ran into a lock {holder}")`, where `holder` refers to a context
in span B. The producer
writes one event. From A, the reader can find the holder; from B, it can find
the event that mentioned it. The argument retains its `holder` role. With no
current span, the explicit reference to B still works. The `$ref` is the link:
there is no separate link event, `add_link` call, or change of parent.

Build this in five increments. Each keeps the flat JSON Lines stream usable
by its reader.

1. **Own the bag; scope only its activation (implemented).** See
   [`context.rs`](crates/sprintr-prototype/src/context.rs). The owned, mutable,
   `!Send` `Context` has an empty
   root, typed lookup, insert/replace, and remove. Own the local handles
   (using `Rc` where ownership is shared) and prepare bindings when the bag
   changes. Allow a context to be stored in a struct, accessed as current,
   installed as a whole, detached, and re-entered. Preserve the existing
   one-binding scope as a convenience. **Done when:** entering an empty root
   hides outer baggage without changing the logger or record counter; nested
   scopes restore on return and unwind. Re-entering a stored context reflects
   its edits; earlier records keep the values they observed.

2. **Give creation an address (implemented).** `context!` records a start
   using the ordinary format grammar and marks the static statement template
   with `log.kind:CONTEXT_START`. It snapshots the active bindings and returns
   a detached context. Its fresh `RecordAddress` identifies the accepted start
   by native thread and record sequence. A rejected write or pre-start stderr
   preview leaves that origin unset, even if the creator has an origin.
   Entering or copying the snapshot emits no extra start. The public macro
   returns a `Context`, not a delivery result.

3. **Implement `span!` and its lifecycle (implemented).** A start statement supplies the
   span's formatted name; its event records the arguments, attributes, start
   time, and parent. A root's `span.start:<spanID>` and
   `trace.start:<traceID>` identify the same start record. A child inherits
   the trace root and gets its own span ID and start record. Enter/exit
   selects the context; closing the span emits its own end statement exactly
   once, referring to the start. There is a single close owner: consume it
   with `end(self)` or let it drop. A `SpanRef` can outlive that owner;
   `into_owner()` explicitly transfers the close right and the span/trace
   anchors. The receiving worker supplies its own baggage and output.
   General context/baggage export is separate. **Done when:**
   a span can be entered twice and closed once, child parentage survives
   replay, and consumers can compute duration from the two timestamps.

4. **Promote references and connect events across workers.** Export an
   immutable `Send + Sync` reference backed by `Arc`, leaving the mutable bag
   on its owning thread. Reuse an accepted origin; for a pending local entity
   that needs an origin, record creation on the originating thread first.
   Accept both local and exported contexts as ordinary log arguments. For
   example, `log.thread:42/span.start:17` selects **record sequence 17** on
   thread 42, not OTel span ID 17. Specify and exercise the corresponding
   record and detached-context targets in the encoder and reader together.
   **Done when:** the lock-holder case works across threads and outputs, and
   one event can reference several contexts without merging their baggage or
   copying their records. Index the event from each context by the same event
   address. A valid target from another, not-yet-loaded stream is unresolved;
   a malformed address is an error.

5. **Carry propagation state; leave transport policy separate.** Keep current
   trace flags and `tracestate` directly available alongside the span and
   trace anchors. An amendment records the change and updates the owner's
   state for subsequent children and exports. All other scoped entity roles
   are local baggage. **Done when:** a child created before an amendment and
   one created after it observe the corresponding states without reading the
   log; an already exported snapshot stays unchanged. In a later transport
   milestone, extract onto an explicit base context and inject from a selected
   context. Plain W3C IDs must work without a sprintr record address. Defer
   cross-process qualification in `tracestate` and baggage selection to that
   milestone.

**Performance gate:** extend the existing allocation checks to warmed context
logging, including an explicit holder reference. Keep process-wide locks,
atomic ownership, and copying whole bags off the ordinary local event path;
measure mutation, first use, and export separately in CI. Task-local async
attachment, durable record retrieval, and OTLP encoding remain later work.

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

sprintr_prototype::start()?;
info!("hello {:s}, value {:04x}", name, value);
```

The equivalent implicit-capture form is
`info!("hello {name:s}, value {value:04x}")`.
Both return `()`. The prototype defaults to
JSON Lines on stdout; a scoped default selects a caller-owned writer.
`info!` encodes borrowed arguments directly into
reusable storage, using a statically encoded format prefix. Heap and
fixed stack buffers reuse the original implementation's storage and
rollback approach. Bounded buffers reject a whole record on overflow,
rather than truncating JSON or dropping arguments.

The application delivers each event to the SDK at the logging call.
The reader decodes an owned record from the resulting stream.

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
    info!("hello {}, value {value:04x}", account_name = name);
}
```

The handle specifies the argument type, allowing a bare `{}`. Attributed
placeholders use canonical names prefixed with `@`, not Rust variable names.
Each callsite defines an anonymous `log.statement` entity whose value
contains static `attrs`, argument bindings in `refs`, `msg`, and `source`:

```json
{"attrs":{"log.level":"INFO"},"refs":[{"account.name":{"$arg":0}}],"msg":"hello {@account.name}, value {value:04x}","source":{"mod":"json_lines","file":"crates/sprintr-prototype/examples/json_lines.rs","line":25,"column":17}}
```

`log.relation:log.statement` describes anonymous object entities;
`log.relation:account.name` describes the named string attribute. A template
contains neither dynamic `args` nor nested `defs`. Its `attrs` contains a
static reference to the named `log.level:INFO` entity. The startup record
already defined `log.statement`, `log.level`, and `log.level:INFO`, so the
first event bootstraps only the remaining definitions. The next event carries
its own address but reuses the statement:

```json
{"seq":1,"stmt":2,"thread":88888,"ts":1704067200123456789,"attrs":{"log.stack":[4336737584,4336770768]},"args":["Ada",42],"defs":{"log.relation:account.name":{"kind":"attribute","type":"string","desc":"Account name"},"log.statement:2":{"attrs":{"log.level":"INFO"},"refs":[{"account.name":{"$arg":0}}],"msg":"hello {@account.name}, value {value:04x}","source":{"mod":"json_lines","file":"crates/sprintr-prototype/examples/json_lines.rs","line":25,"column":17}}}}
{"seq":2,"stmt":2,"thread":88888,"ts":1704067200124567890,"attrs":{"log.stack":[4336737584,4336770768]},"args":["Grace",42]}
```

`"stmt":2` is a typed reference to `defs["log.statement:2"]`.
The nonzero integer identity is assigned on first use and is process-local,
not a globally qualified reference; the startup announcement used `1`. Each
callsite has its own identity, even when its template is equal to another's.
After the announcement, rendering produces:

```text
2024-01-01T00:00:00.123Z hello Ada, value 002a
log.level = INFO, account.name = Ada
2024-01-01T00:00:00.124Z hello Grace, value 002a
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
are errors. `info!`, `warn!`, and `error!` bake `INFO`, `WARN`, or `ERROR`
into their statement templates.
The named values carry the descriptions "Informational event", "Warning
condition", and "Error condition". Each output emits a level definition only
when first needed, and reuses it across statements. Application context
associations live on events independently of these templates.
The prototype supports string and `u64` identity attributes, global and
container-scoped named entities, anonymous runtime objects, statement `$arg`
bindings, and record-relative references to whole object definitions. General
graph linking, filtering, and graph navigation remain future work.

## Additional context

https://gist.github.com/jmacd/17a6f2885d92d7864ba76680a8ee367a
