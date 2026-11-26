# Standalone sprintr prototype

This non-production prototype captures a message format and typed positional
arguments, records JSON Lines, and replays plain-text messages. Declared
attributes, statically named entities such as levels, and anonymous objects
supply self-describing metadata. Thread-local context adds entity associations
to events. Filtering and binary encoding are future work.
There is no dependency on Tokio, tracing, or an OpenTelemetry runtime.

## Run the example

From the repository root:

```sh
cargo run --quiet -p sprintr-prototype --example json_lines -- record
```

After the [startup announcement](#startup-announcement):

```json
{"seq":1,"stmt":2,"thread":88888,"ts":1704067200123456789,"attrs":{"log.stack":[4336737584,4336770768]},"args":["Ada",42],"defs":{"log.relation:account.name":{"kind":"attribute","type":"string","desc":"Account name"},"log.statement:2":{"attrs":{"log.level":"INFO"},"refs":[{"account.name":{"$arg":0}}],"msg":"hello {@account.name}, value {value:04x}","source":{"mod":"json_lines","file":"crates/sprintr-prototype/examples/json_lines.rs","line":25,"column":17}}}}
{"seq":2,"stmt":2,"thread":88888,"ts":1704067200124567890,"attrs":{"log.stack":[4336737584,4336770768]},"args":["Grace",42]}
```

Install the reader from the repository root and replay from stdin:

```sh
cargo install --locked --path crates/sprintr-cli
cargo run --quiet -p sprintr-prototype --example json_lines -- record \
  | sprintr replay
```

```text
2024-01-01T00:00:00.122Z process 50316 started logging with sprintr 0.1.0
log.level = INFO, process.pid = 50316, telemetry.sdk.name = sprintr, telemetry.sdk.version = 0.1.0
2024-01-01T00:00:00.123Z hello Ada, value 002a
log.level = INFO, account.name = Ada
2024-01-01T00:00:00.124Z hello Grace, value 002a
log.level = INFO, account.name = Grace
```

For a tour that prints each JSON record immediately followed by its indented
rendering, run
`cargo run --quiet -p sprintr-prototype --example tour`.

The [DFE hierarchy example](examples/dfe_hierarchy.rs) models one engine and
an `ingest` group with `logs` and `metrics` pipelines. Each pipeline has a
`batch` node with a different qualified identity. Two workers each process
one batch: dispatch starts a fresh trace, and the group, pipeline, and node
operations are child spans. Filter the JSON Lines by trace ID to follow
one batch even when the workers' records interleave, or replay both as text:

```sh
cargo run --quiet -p sprintr-prototype --example dfe_hierarchy \
  | sprintr replay
```

With `--fail-metrics`, the logs batch still succeeds. The metrics node records
`NodeError::OutputClosed` in its execution span. Back in the pipeline span,
`PipelineError::BatchRejected` links that cause to batch 2. The engine's
dispatch span records a warning containing just the wrapper reference.
To expand that warning into the two failure-site stacks:

```sh
cargo run --quiet -p sprintr-prototype --example dfe_hierarchy -- --fail-metrics \
  | cargo run --quiet -p sprintr-prototype --example exception_chain -- replay
```

The reader requires only the recorded JSON, not the original application.
Every malformed record or unsupported format stops playback with its input
line number. Input, output, and flush failures are returned rather than ignored.

## Try the API

```rust
use sprintr_prototype::{info, start};

let name = String::from("Ada");
let value = 42u32;

start()?;
info!("hello {name:s}, value {value:04x}");
info!("hello {:s}, value {:04x}", name, value);
# Ok::<(), std::io::Error>(())
```

To record a synchronous operation into a caller-owned writer, install it once
for the scope. Calls made by other functions on that thread use it too:

```rust
use sprintr_prototype::{Logger, info, replay};

fn greet(name: &str) {
    info!("hello {name:s}");
}

let mut logger = Logger::new(Vec::new());
logger.start()?;
logger.with_default(|| greet("Ada"));
logger.flush()?;

// Replay the records the logger wrote.
let mut text = Vec::new();
replay(logger.into_inner().as_slice(), &mut text)?;
assert!(text.ends_with(b"hello Ada\nlog.level = INFO\n"));
# Ok::<(), std::io::Error>(())
```

`info!` returns `()`. It borrows string data and encodes it
directly without requiring `Display` or allocating intermediate owned arguments.
The caller retains ownership. Expressions are evaluated once per placeholder
occurrence. Each logging call delivers the event to the SDK immediately.
`Message` is the reader's owned representation of a decoded record.
Writers are synchronous, and I/O failure may leave a partially written line.
Call `Logger::flush` explicitly for buffered output.

## Structured arguments

Implement `StructuredValue` on the type you want to log. Its `encode` method
can use `structured!` to describe how to display its fields. Callers pass the
object directly to `{}`; no adapter or intermediate label is needed.

```rust
use sprintr_prototype::{StructuredValue, ValueWriter, info, structured};

struct Point { x: u64, y: u64 }
impl StructuredValue for Point {
    fn encode(&self, out: &mut dyn ValueWriter) -> std::io::Result<()> {
        let Self { x, y } = self;
        structured!("({x}, {y})").encode(out)
    }
}

sprintr_prototype::start()?;
let point = Point { x: 3, y: 4 };
info!("position {point}");
# Ok::<(), std::io::Error>(())
```

The event renders as `position (3, 4)`. First use on each output and thread
includes the named encoder definition (record excerpt):

```json
{
  "args": [[3,4]],
  "refs": [{"struct.encode:Point_f26ff73e147c5d02":"#/args/0"}],
  "defs": {
    "struct.encode:Point_f26ff73e147c5d02": {
      "msg":"({x}, {y})", "name":"example::Point", "file":"src/main.rs", "line":8
    }
  }
}
```

Later events carry the same `refs` entry and new `args`. The key uses the
unqualified type without generic arguments plus a hash of the definition.
The definition retains the full concrete type and the location of `structured!`.

For named fields instead, the same trait method can call
`out.object("Point", &[("x", Value::Unsigned(self.x)), ("y", Value::Unsigned(self.y))])`.
The call site stays the same; `args[0]` becomes `{"x":3,"y":4}` with a
`struct.name:Point` reference. `out.array` writes a plain array. Object fields can hold another
`Value::Structured` or an entity handle (`Value::Entity`). Attributes label
positional arguments of `structured!` or a log statement. Inside `structured!`,
bare strings and unsigned integers also work, with
integers rendered in decimal. Formatting markers and associations are recorded
on the outer event. An entity's first use supplies its definition there.
`:?` accepts a borrowed `dyn Debug` and stores its Debug **text** as a JSON
string; use `StructuredValue` to expose individual fields.

Pass children to the writer rather than recursively calling a child's `encode`
yourself. If a child is the same active nonzero-sized node, its place in `args`
holds a `$ref` back to the ancestor. Independent sibling uses are encoded
normally. Traversals deeper than 32 structured values fail the whole record.

### A value that is an entity, or contains one

```rust
use sprintr_prototype::{attributes, entities, info, structured, EntityHandle, StructuredValue, ValueWriter};

entities! {
    /// Application session
    #[anonymous]
    session = "session";
}
attributes! {
    /// Session handling the request
    active_session: entity(session) = "request.session";
}

struct Session(EntityHandle);
impl StructuredValue for Session {
    fn encode(&self, out: &mut dyn ValueWriter) -> std::io::Result<()> {
        self.0.encode(out)
    }
}
struct Request<'a> { entity_handle: &'a EntityHandle }
impl StructuredValue for Request<'_> {
    fn encode(&self, out: &mut dyn ValueWriter) -> std::io::Result<()> {
        structured!("handling {}", active_session = self.entity_handle).encode(out)
    }
}

sprintr_prototype::start()?;
let login = Session(session.new(serde_json::json!({"name":"Ada"}))?);
info!("{login}");
let request = Request { entity_handle: &login.0 };
info!("{request}");
# Ok::<(), std::io::Error>(())
```

`Session` delegates directly: `args[0]` holds only a `$ref` to its definition.
`Request` emits a formatted array containing that same reference. If `session:9`
was defined in record 1, the second event has these fields (hash abbreviated):

```json
{
  "args": [[{"$ref":"1#/defs/session:9"}]],
  "refs": [
    {"struct.encode:Request_abcd":"#/args/0"},
    {"request.session":"#/args/0/0"}
  ]
}
```

The encoder names the layout of the array; `request.session` labels its first
argument. That argument's `$ref` already identifies `session:9` and the record
that defines it.

An outer statement can likewise use `info!("handling {}", active_session = login.0)`.
There the attribute binding lives in the statement template; the event needs
only the `$ref` in its argument slot. The attribute's schema records its
target as `"type":{"entity":"session"}`.

## Static levels

```rust
use sprintr_prototype::{error, info, start, warn};

start()?;
info!("ready");
warn!("retrying");
error!("failed");
# Ok::<(), std::io::Error>(())
```

The macros share the same capture syntax and return `()`.
They bake `{"log.level":"INFO"}`, `{"log.level":"WARN"}`, or
`{"log.level":"ERROR"}` into the statement's `attrs`. `log.level` is a named
object-entity relation, not a string attribute. A value such as `"INFO"` is a
typed reference to `defs["log.level:INFO"]`, whose value supplies its description:

| Entity | Description |
|--------|-------------|
| `log.level:INFO` | Informational event |
| `log.level:WARN` | Warning condition |
| `log.level:ERROR` | Error condition |

The relation and each used named value are emitted once per output stream,
alongside the first statement that needs them; the startup announcement already
carries `log.level:INFO`. Unused levels are not emitted. Another statement at
the same level reuses its definition, and subsequent occurrences still contain
only the address, statement reference, and arguments. Definitions are immutable; missing
or conflicting named values fail playback.

The levels are declared with [`entities!`](#declaring-attributes-and-entities)
in `sprintr_prototype::schema`, like any other relation. Playback recognizes
named entities generically, not only levels.

The level comes from static statement metadata. Scoped context adds separate
event attributes; it leaves the level unchanged.
Positional `$arg` associations still bind to each event's argument values.
Playback includes the level first on the association line, for example
`log.level = INFO, account.name = Ada`.

## Scoped entity context

Declare an entity-valued attribute for each role. Prepare its association once
and enter it around ordinary application code:

```rust
use sprintr_prototype::{Context, attributes, entities, info, start};

entities! {
    /// Application session
    #[anonymous]
    session = "session";
}
attributes! {
    /// Session handling the request
    active_session: entity(session) = "request.session";
}

struct Request { context: Context }
fn serve() { info!("request received"); }

start()?;
let request = {
    let login = session.new(serde_json::json!({"account":"Ada"}))?;
    let mut context = Context::root();
    context.insert(active_session.context(&login)?);
    Request { context }
};
request.context.in_scope(serve);
# Ok::<(), std::io::Error>(())
```

For `session:1`, the event's `attrs` contains
`"request.session":{"$ref":"session:1"}` alongside `log.stack`. This `$ref`
is an immutable entity key: playback looks it up in the current or earlier
definitions in the stream. Positional argument references still address a
specific record and JSON Pointer. A declared named entity works the same way,
for example `deployment.context(&environment::PRODUCTION)?`.

`Context::current()` detaches an owned snapshot, or returns an empty root outside
a scope. Clones share prepared, immutable bindings; inserting a replacement or
removing a role affects only the edited snapshot. Use `context.get(&active_session)?`
to inspect a role and `context.remove(&active_session)?` to remove it.
`context.in_scope(f)` replaces the *whole* current bag until `f` returns or
unwinds; `Context::root().in_scope(f)` therefore clears all inherited roles.
For a one-role overlay, `active_session.context(&login)?.in_scope(f)` retains
the other current roles. Neither operation changes the selected logger.
Contexts can be stored and re-entered on their owning thread.

### Recording a context's origin

```rust
use sprintr_prototype::{context, info, start};

start()?;
let id = 42u32;
let job = context!("new job {id:04x}");
job.in_scope(|| info!("processing"));
# Ok::<(), std::io::Error>(())
```

`context!` takes a snapshot of the active bindings and records one creation
event without entering the new context. The format and typed arguments work
as in `info!`. Its statement template carries `"attrs":{"log.kind":"CONTEXT_START"}`;
the first use defines the named kind. Playback can recognize subsequent starts
from their statement ID without inspecting the message text.

`job.origin()` is the accepted creation record's `(thread, sequence)` as a
`RecordAddress`. Cloning, editing, or re-entering the snapshot preserves that
origin without emitting another start. `Context::root()` has no origin.
If the output rejects the record, the SDK diagnoses the failure and returns
a usable context with its inherited bindings and no origin. A pre-start stderr
preview likewise has no origin. Acceptance means the writer took the complete
record, not that buffered output reached durable storage.

### Starting, entering, and ending a span

```rust
use sprintr_prototype::{info, span, start};

start()?;
let batch = 42u32;
let job = span!("process batch {batch:04x}");
job.in_scope(|| {
    let read = span!("read input"); // child of job
    read.in_scope(|| info!("reading"));
    // Dropping read records its end.
});
job.in_scope(|| info!("publishing")); // same span, second entry
job.end(); // close now instead of on drop
# Ok::<(), std::io::Error>(())
```

`span!` uses the ordinary format and argument grammar and returns one local
owner. Its typed `SPAN_START` record supplies the name, arguments, attributes,
and timestamp. Entering and leaving only switch the current context. An
explicit `end(self)` or dropping the owner attempts one distinct `SPAN_END`
statement; duration is derived from the two timestamps. End uses the output
selected **at close**, so keep a scoped logger installed through close when
both records should go to that logger. A rejected or preview-only start
still returns a usable local scope, but publishes no start reference or end.

The start and events in the span carry `refs` like these (IDs are hex; the
address components are decimal):

```json
[
  {"span.start:000000000000000a":"log.thread:42/span.start:17"},
  {"trace.start:0000000000000000000000000000000b":"log.thread:42/span.start:17"}
]
```

Here record 17 is the root start. A child has a fresh span ID, inherits the
root reference, and its start also carries `span.parent:<parentID>` pointing
to the immediate parent start. `Message::span_links()` exposes these roles;
`Renderer::span_start(address)` returns an accepted start's metadata.

`job.reference()` gives an optional `SpanRef`: it is `Send + Sync`, loggable
as `info!("blocked on {holder}")`, and remains valid after close. The argument
contains only `{"$ref":"log.thread:42/span.start:17"}`. To give a worker the
**close right** instead, consume the span with `job.into_owner()`. The
resulting `SpanOwner` can move, cannot be cloned, and closes on drop or
explicit `end()`. Its `in_scope` and close use the destination's selected
logger and baggage, retaining the original span and trace references. This
handoff does not transfer local entity handles.

### Capturing and referring to errors

Give an ordinary Rust error variant a `StructuredError` payload. `exception!`
returns that variant, so callers can match it or return it with `Err(...)`.
`thiserror` is optional: `#[error(transparent)]` delegates `Display` and
`Error::source` to the payload without declaring the message twice.

```rust
use sprintr_prototype::{exception, warn, StructuredError};
use thiserror::Error;

#[derive(Debug, Error)]
enum LoadError {
    #[error(transparent)]
    BadPort(StructuredError),
    #[error(transparent)]
    Config(StructuredError),
}

# sprintr_prototype::start()?;
let port = "metrics";
let LoadError::BadPort(inner) = exception!(LoadError::BadPort, "unknown output {port:s}")
    else { unreachable!() };
let path = "agent.yaml";
let err = exception!(LoadError::Config, "could not load {path:s}", cause = &inner);
assert_eq!(err.to_string(), "could not load agent.yaml");
if let LoadError::Config(problem) = &err {
    warn!("startup failed: {problem}");
}
# Ok::<(), std::io::Error>(())
```

This writes **two exception occurrences and one warning**. Each occurrence
has its own UID, typed arguments, active context/span, source location and
raw caller PCs. The warning's argument is just a `$ref` to the wrapper. The
optional trailing `cause` refers to an earlier occurrence and consumes no
placeholder. Use a qualified attribute path if an ordinary field is also
named `cause`.

The local `StructuredError` owns its fields and local cause chain through
`Rc`. Local display formats the captured values on demand, even after the
writer is gone. Exception fields support strings, unsigned integers, declared
scalar and entity attributes, entity/span/exception handles, and explicit
`:?` (Debug text captured once). Borrowed `StructuredValue` formatters are
available in ordinary logs; in an exception, pass their identifying handle
or the fields you want to retain.

`problem.reference()` promotes an accepted occurrence to a `Send + Sync`
`ExceptionRef`. Send it to a worker to log it or to wrap it with
`cause = &reference`. Its local `Display` is the address; playback can
retrieve the original message and stack. Rejected writes and pre-start
diagnostics leave `reference()` as `None`. The application still receives its
error, and a later wrapper explicitly identifies the unrecorded cause by UID.

**Example path:** `crates/sprintr-prototype/examples/exception_chain.rs`.
Record in one process and expand the reported error in another:

```sh
cargo run -q -p sprintr-prototype --example exception_chain -- record \
  | cargo run -q -p sprintr-prototype --example exception_chain -- replay
```

`Renderer::exception_chain(origin, |_| None)` prints `Caused by:` with each
occurrence's own frame zero and raw PCs, abbreviating common caller frames.
A resolver callback can supply frame text using the **originating process's**
module map and debug artifacts. `Some(symbols.format_frame(pc))` with
`LocalSymbols` is appropriate only for a reader in the same execution.

## Declaring attributes and entities

```rust
use sprintr_prototype::{attributes, entities, info};

attributes! {
    /// Account name
    pub account_name: str = "account.name";
    /// Retry attempt
    pub retry_count: u64 = "retry.count";
}

entities! {
    /// Deployment environment
    #[named(global)]
    pub environment = "deployment.environment" {
        /// Production traffic
        PRODUCTION,
        /// Pre-production testing
        STAGING,
    }
}

let name = "Ada";
sprintr_prototype::start()?;
info!("hello {}, attempt {}", account_name = name, retry_count = 2u8);
assert_eq!(environment::STAGING.description(), "Pre-production testing");
# Ok::<(), std::io::Error>(())
```

Each doc comment is the recorded description and is required. Attribute values
are `str`, `u64`, or an entity of a relation declared with `entity(relation)`.
Entity relations hold object values; statically named values
such as `log.level:INFO` are the unusual case.
`#[anonymous]` marks a relation whose values have no names, such as
`log.statement`. A relation with named values also declares a module of the
same name holding them.

`account_name = name` associates the next positional placeholder with a declared
attribute; it is not a named formatting argument or an assignment to the handle.
The handle supplies the type, so a string attribute accepts `{}` or `{:s}` and
an unsigned one accepts `{}` (decimal) or `{:x}`. The stored placeholder is
`{@account.name}` regardless of the Rust variable or import alias. Canonical
names cannot be empty, be `log.relation`, or contain braces/colons, which would
conflict with placeholder syntax. Handles must be declared statics, not locals.

Declarations are pre-encoded at compile time and collected at link time. They
emit nothing by themselves. Names are process-wide, not namespaced; dots have
no scoping meaning, and equal declarations in different crates share a name.
sprintr's own relations (`log.statement`, `log.level`, `process.pid`,
`telemetry.sdk.*`) are declared the same way in `sprintr_prototype::schema`;
only `log.relation`, the relation of relations, is built into the wire format.

### Globally and locally named entities

Declare the identifying attributes separately from the descriptive object.
`within` names a relation in the declaration and takes a concrete container
at construction. Entering a logger or entity context does not select the
identity scope.

```rust
use sprintr_prototype::{attributes, entities, info, start};

attributes! {
    /// Pipeline instance UID
    run_uid: str = "pipeline_instance.uid";
    /// Node ID within this pipeline instance
    node_id: str = "node.id";
}
entities! {
    /// Pipeline instance
    #[named(global, id = run_uid)]
    pipeline_instance = "pipeline_instance";
    /// Runtime node
    #[named(within = pipeline_instance, id = node_id)]
    node = "node";
}

start()?;
let run = pipeline_instance.named("run-42", serde_json::json!({}))?;
let batch = node.within(&run).named("batch", serde_json::json!({"kind":"processor"}))?;
info!("starting {batch}");
# Ok::<(), std::io::Error>(())
```

The resulting key is `node:[["run-42"],["batch"]]`. The relation definitions
carry `id_scope`, the ordered `id_keys`, and (for local names) `scope_entity_type`.
Each component of the key contains its typed ID values in declaration order.
Compound IDs use `id = (first_attr, second_attr)` and accept a pair, or an
array of `IdentityValue` for longer mixed keys. Wrong containers, arities and
scalar types fail at construction. The parent can be dropped, and the child
can be decoded even if the parent's object was never logged.

For `within = sprintr_prototype::schema::span_start`, pass a `&SpanRef`.
The child's identity includes `trace.id` and `span.id`; two traces may use the
same SpanID. The start-record address locates the observation separately.

For a direct encoder, `handle.identity()` exposes `declaration().id_keys`,
`values()`, and `within()` as Rust data. JSON is one projection of that
identity. The output deduplicates equal handles by their qualified identity;
their record-relative `$ref` still identifies a particular accepted definition.
`#[named(global)]` with a block declares static vocabulary such as `INFO`;
`#[anonymous]` requests a library-assigned process-local UID.

### Defining and reusing an anonymous object

```rust
use serde::Serialize;
use sprintr_prototype::{entities, info};

entities! {
    /// A user session; its values have identities, not names
    #[anonymous]
    session = "session";
}

#[derive(Serialize)]
struct Login<'a> { account: &'a str, method: &'a str }

let login = session.new(Login { account: "Ada", method: "passkey" })?;
sprintr_prototype::start()?;
info!("opened {}", login);
info!("resumed {}", login);
# Ok::<(), std::io::Error>(())
```

`new` snapshots a serializable **object** and assigns an identity. It does not
log. For example, if `session:1` first appears in record 1, the `opened` event
includes `"session:1":{"account":"Ada","method":"passkey"}` in `defs`.
Both events have `"args":[{"$ref":"1#/defs/session:1"}]`. The number before
`#` selects record 1 **on the event's thread**; the fragment selects that
record's definition. Playback displays the resolved object, not the pointer.
Another output on the same thread defines the object on first use there.
Local handles share immutable bytes through `Rc`; a transferable origin
reference is planned separately.

### Collisions

The compiler cannot check declarations across independently compiled crates,
and a name collision does not stop the process. When logging first starts, all
declarations are resolved once. sprintr's schema is resolved first; afterwards,
the first declaration of a name keeps it, in link order, which is unspecified.
A later, different declaration is recorded under a content-derived name such as
`account.name_1a2b3c4d`, and its statements use that name. Each output stream
announces every rename in a `WARN` record right after startup:

```text
attribute account.name (Legal name of the account holder) collides with an earlier declaration; recorded as account.name_1a2b3c4d
log.level = WARN
```

## Startup announcement

Call `start()` once for stdout or `logger.start()` for a caller-owned output.
`logger.with_default(f)` selects that output for `f` on the current thread.
Nested scopes restore the previous output on return or panic. Spawned threads
use their own default; the scope does not follow a future across `.await`.
Before startup, a logging call prints the message and an unstarted-SDK warning
to stderr. Startup is fallible; a repeated explicit call returns the original
outcome. After a partial startup failure, ordinary calls continue best-effort.
Encoding and sink errors are diagnosed on stderr, not returned to instrumentation.

Startup begins its output with an `INFO` record announcing
the process and the library, followed by any collision warnings. Stdout announces
once per process. Every emitted event has `thread` (the native unsigned thread
ID) and `seq` (the record ID within that thread). A thread's first record is
`seq:0` and defines `log.thread:<id>`. All outputs on the thread share one
counter; another output introduces the thread again without restarting at zero.
The writer does not track OS ID reuse. Playback reports a repeated or
decreasing record number for the same thread rather than silently linking it.
Sequence numbers may have gaps; a failed write cannot leave a later reference
pointing at an unaccepted definition.

## Statement templates and events

Every `info!`, `warn!`, or `error!` callsite has a static anonymous `log.statement`
entity. Its static fields are pre-encoded:

```json
{
  "attrs": {"log.level": "INFO"},
  "refs": [{"account.name": {"$arg": 0}}],
  "msg": "hello {@account.name}, value {value:04x}",
  "source": {"mod":"json_lines","file":"crates/sprintr-prototype/examples/json_lines.rs","line":25,"column":17}
}
```

The template contains no `args` or nested `defs`. `{"$arg": 0}` binds to the
first argument of each event using it; it is not a pointer into the template's
record. Ordinary JSON Pointer semantics are unchanged. Repeated associations
remain separate `refs` entries. Named level references live in static template
`attrs`; the stack is captured separately for each event.

`seq`, `stmt`, and `thread` are wire shorthand for `attrs["log.record"]`,
`attrs["log.statement"]`, and `attrs["log.thread"]`. Readers accept either spelling, but reject records
that supply both for the same value. In memory, `Message::attrs` uses the full
names.

Each record also carries `ts`, the wall-clock capture time as a `u64`
count of Unix nanoseconds. Playback displays UTC to milliseconds, truncating
sub-millisecond digits only for display. Use `seq` for ordering on a thread.

First use assigns a process-local nonzero `u64` statement identity. `stmt`
is a typed reference to that entity, resolved through `defs["log.statement:<id>"]`.
IDs are not globally qualified references and must not be conflated across
processes. Distinct callsites have distinct identities even for equal templates.
The first occurrence carries its statement, attribute, and required
relation/level definitions not already sent in the same stream. Later
occurrences with no new definitions contain only `seq`, `stmt`, `thread`, `ts`, `attrs`, and `args`. New callsites introduce their own
statement definitions but need not repeat already-emitted relations or levels.

Each caller-owned logger tracks first use independently. Stdout tracks it per
thread alongside its scratch buffer, so different threads may repeat identical
definitions. Definitions are marked emitted only after the output accepts the
record.

`replay` retains definitions and compiles each template once into formatting
operations and argument bindings; `Renderer` exposes the same behavior directly.
It binds each occurrence without copying an expanded record. Playback renders the message followed by an
association line (`log.level = INFO, account.name = Ada`), with multiple associations separated
by commas. The association line escapes control characters. Unknown relations,
conflicting definitions, type mismatches, and invalid bindings stop playback.
The prototype implements `$arg` bindings and references to whole anonymous
objects in `defs`, not arbitrary pointer traversal or cross-process lookup.
The earlier per-record `msg`/`refs` wire format is no longer accepted.

### Call stacks and symbols

**Frame 0** is the statement's `source`: Rust's `module_path!()`, `file!()`,
`line!()`, and `column!()` at the logging macro invocation, pre-encoded with
the message format. It needs no debug information or runtime lookup.

`attrs["log.stack"]` starts at **frame 1**: up to 10 native caller PCs by
default on macOS and glibc Linux, innermost first. The producer calls libc's
`backtrace()` directly into a stack-local buffer; it takes no SDK-wide lock
and looks up no symbols. Other targets retain frame zero and emit an empty
caller list; requesting a positive limit there returns `Unsupported`.
To disable unwinding process-wide, call
`sprintr_prototype::set_stack_limit(0)?;`. The event then carries `[]` and
the statement keeps its source. Limits from 0 to 64 fit the producer's
fixed-size capture buffer.
Each PC identifies an anonymous `log.source` in that process; there are no
`log.source:<pc>` records. Reaching the same statement through a different
caller can change the stack without changing `stmt`.

The `tour` example decodes a record before asking `symbols::LocalSymbols` for
module, demangled function, source file, and line. Its `log.stack =` display
uses `at call_once (src/ops/function.rs:250)` without generic arguments.
Paths start at the nearest `crates/` or `src/` directory, then drop whole
leading components to fit 32 characters; a long filename stays intact.
Lookup is cached by PC and
may include inlined frames. Plain replay does not load symbols. For line
information, keep the exact binary and its debug info (e.g. the matching dSYM
on macOS); for release builds use `CARGO_PROFILE_RELEASE_DEBUG=1`. Missing
debug information leaves the PC intact.

For **offline** playback, save the originating process's loaded modules in
one optional sidecar, not one symbol entry per address:

```sh
cargo run --quiet -p sprintr-prototype --example json_lines -- record modules.json > records.jsonl
sprintr replay --module-map modules.json records.jsonl
```

This prints the module and its linked call-instruction address for each PC
on each event. Pass that address to `llvm-symbolizer
--obj=<matching-binary> <linked-address>` for names and lines. The sidecar
contains executable segment ranges, their link-time addresses and debug IDs
where available, not the debug sections themselves. Use the map from the
**same run**: ASLR changes addresses even for identical binaries. Take the
snapshot while the images are loaded; dynamic unload/reload is not tracked.

### Replay using only the recording

For a self-contained recording, build a **standalone GSYM v1** pack for each
image you want to symbolize. This is a build/release step; the exact executable
and its debug information are required here, not at replay. For the DFE demo:

```sh
CARGO_PROFILE_DEV_DEBUG=2 cargo build -p sprintr-prototype --example dfe_hierarchy
exe=target/debug/examples/dfe_hierarchy
# ELF with embedded DWARF (Linux):
llvm-gsymutil --convert="$exe" -o dfe.gsym
# On macOS use these two commands instead:
# dsymutil --flat "$exe" -o dfe.dwarf
# llvm-gsymutil --convert=dfe.dwarf -o dfe.gsym
"$exe" --symbols dfe.gsym --fail-metrics > run.jsonl
```

Copy only `run.jsonl` to the reader. Both commands below use the embedded
bytes; neither opens paths named in the recording.

```sh
cargo run -q -p sprintr-prototype --example exception_chain -- replay < run.jsonl
sprintr replay --symbols run.jsonl
```

Applications call `symbols::record_symbols(paths)?` once on the started
output, before starting workers. Repeat `--symbols FILE` in the demo to cover
shared libraries. Duplicate copies of a pack cost one payload. Unmatched
build IDs or corrupt pack data produce errors; uncovered frames retain the
address, module and build ID when available. This prototype accepts
little-endian GSYM v1 and assumes the executable mappings stay stable.
Enabling it includes function names and build-time source paths in the log:
apply the same access and retention policy as for debug artifacts.

## Producer allocation profile

The proc macro parses formats and pre-encodes immutable statement and relation
values at compile time. `info!` constructs a stack array of borrowed arguments and writes
JSON directly into reusable storage. It does not construct an owned `Message`,
copy strings into temporary allocations, or parse the format again at runtime.

Storage and rollback are adapted from the original direct OTLP implementation.
`Logger::new` preallocates 2 KB, enough for the startup announcement, and
retains its buffer between records. Larger records can grow it; once sufficient
capacity and first-use definition bookkeeping are available, successful
recording performs zero library heap allocations. Declarations are resolved
once per process, and first use assigns the statement's identity once;
subsequent recording neither locks the registry nor re-encodes definitions.
By default stdout uses the
same approach with a per-thread scratch buffer.
Encoding happens before acquiring the stdout lock.

Fixed stack storage avoids the initial heap-buffer allocation, but first-use
definition tracking can still allocate. Bounded storage must fit the startup
announcement; 2 KB is sufficient, and a smaller buffer fails loudly with
`StorageFull` from `start()` if the announcement does not fit:

```rust
use sprintr_prototype::{Logger, buffer::StackBuffer, info};

let mut logger = Logger::with_buffer(std::io::sink(), StackBuffer::<2048>::new());
logger.start()?;
let name = "Ada";
let value = 42u32;
logger.with_default(|| info!("hello {name:s}, value {value:04x}"));
# Ok::<(), std::io::Error>(())
```

`HeapBuffer::with_capacity_and_limit` also permits an explicit record-size
limit. Limits count the complete encoded line, including JSON escaping and
the newline. An oversized event produces a diagnostic without writing any
of it to the output, and the buffer can be reused. No positional arguments
are silently dropped and no JSON strings are byte-truncated.

Allocation tests cover warmed heap and stack storage and repeated attributed
statements with changing values. The
zero-allocation target excludes work done by user argument expressions and
output sinks, first-use schema validation/bookkeeping, buffer initialization/growth,
and error reporting. Playback and graph-building may allocate. Throughput,
contention, and retained-memory policies still need production evaluation.

## Format subset

- `:s` captures an `AsRef<str>` value. Its stored placeholder omits `:s`.
- A bare `{}` borrows a structured value or an anonymous object handle. For a handle the argument
  on the wire is a reference; playback displays the resolved object as JSON.
- `{}` also captures a value when the positional argument uses a declared
  attribute handle, such as `account_name = name`; its stored label is
  `@account.name`. Unsigned attributes render `{}` in decimal.
- `:x` captures a `u8`, `u16`, `u32`, or `u64` value without formatting it first.
  `#` includes `0x`, `0` enables zero padding, and a constant width specifies
  the minimum width including any prefix. For example, `:04x` renders `002a`
  and `:#04x` renders `0x2a`. Width is limited to 4096.
- `{{` and `}}` encode literal braces. Argument data is never parsed as a format.
- Implicit captures name local variables; empty source placeholders consume
  explicit arguments. Unattributed labels come from source identifiers or
  expressions; attributed labels use canonical names. All placeholders consume
  `args` in occurrence order, not by name lookup.

Bare `{name}` string capture in a log statement, signed integers, floats, named argument
overrides, and indexed placeholders are deliberately unsupported. Bind explicit
expressions containing braces or colons to local variables first, so their
stored labels cannot be mistaken for placeholder syntax. Invalid source formats,
wrong argument types, and missing or surplus arguments are compilation errors.
JSON retains the full `u64` range; the playback reader does not convert it to
floating point.

The small `sprintr-message` crate shares the format grammar between the capture
proc macros (`sprintr-prototype-macros`) and playback. `sprintr-prototype` exposes
the public macros, caller-owned logger, and reader. These APIs are experimental.
