# Standalone sprintr prototype

This non-production prototype captures a message format and typed positional
arguments, records JSON Lines, and replays plain-text messages. Declared
attributes, statically named entities such as levels, and anonymous objects
supply self-describing metadata. There is no thread-local application
attribute injection, filtering, or binary encoding yet.
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

Replay from stdin:

```sh
cargo run --quiet -p sprintr-prototype --example json_lines -- record \
  | cargo run --quiet -p sprintr-prototype --example json_lines -- replay
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

The level comes from static statement metadata. There is no dynamic override,
filtering, or thread-local application attribute injection yet.
Positional `$arg` associations still bind to each event's argument values.
Playback includes the level first on the association line, for example
`log.level = INFO, account.name = Ada`.

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
are `str`, `u64`, or a handle to an anonymous relation declared with `entity(relation)`.
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
Another output or thread defines the object on first use there, so a short
reference never needs another stream.

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
cargo run --quiet -p sprintr-prototype --example json_lines -- replay modules.json < records.jsonl
```

This prints the module and its linked call-instruction address for each PC
on each event. Pass that address to `llvm-symbolizer
--obj=<matching-binary> <linked-address>` for names and lines. The sidecar
contains executable segment ranges, their link-time addresses and debug IDs
where available, not the debug sections themselves. Use the map from the
**same run**: ASLR changes addresses even for identical binaries. Take the
snapshot while the images are loaded; dynamic unload/reload is not tracked.

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
