# Standalone slogr prototype

This non-production prototype captures a message format and typed positional
arguments, records JSON Lines, and replays plain-text messages. Static
string attributes and anonymous statement entities supply self-describing metadata.
There are no generic entities, severity fields, filtering, or binary encoding yet.
There is no dependency on Tokio, tracing, or an OpenTelemetry runtime.

## Run the example

From the repository root:

```sh
cargo run --quiet -p slogr-prototype --example json_lines -- record
```

```json
{"attrs":{"log.statement":1},"args":["Ada",42],"defs":{"log.relation:log.statement":{"kind":"entity","type":"object","desc":"Logging statement","anonymous":true},"log.relation:account.name":{"kind":"attribute","type":"string","desc":"Account name"},"log.statement:1":{"attrs":{},"refs":[{"account.name":{"$arg":0}}],"msg":"hello {@account.name}, value {value:04x}"}}}
{"attrs":{"log.statement":1},"args":["Grace",42]}
```

Replay from stdin:

```sh
cargo run --quiet -p slogr-prototype --example json_lines -- record \
  | cargo run --quiet -p slogr-prototype --example json_lines -- replay
```

```text
hello Ada, value 002a
account.name = Ada
hello Grace, value 002a
account.name = Grace
```

The reader requires only the recorded JSON, not the original application.
Every malformed record or unsupported format stops playback with its input
line number. Input, output, and flush failures are returned rather than ignored.

## Try the API

```rust
use slogr_prototype::{Logger, info, message};

let name = String::from("Ada");
let value = 42u32;

// JSON Lines to stdout, with implicit captures.
info!("hello {name:s}, value {value:04x}")?;

// Caller-owned output, with explicit positional expressions.
let mut logger = Logger::new(Vec::new());
info!("hello {:s}, value {:04x}", logger: logger, name, value)?;
logger.flush()?;

// Capture and rendering are independent of delivery.
let record = message!("hello {name:s}, value {value:#04x}");
assert_eq!(record.render()?, "hello Ada, value 0x2a");
# Ok::<(), Box<dyn std::error::Error>>(())
```

`info!` returns `std::io::Result<()>`. It borrows string data and encodes it
directly without requiring `Display` or allocating intermediate owned arguments.
The caller retains ownership. Expressions are evaluated once per placeholder
occurrence. `message!` is a separate, allocating API for owned snapshots.
Its snapshots include their definitions; conflicting static declarations panic
on this infallible convenience path. `info!` returns schema conflicts as errors.
Writers are synchronous, and I/O failure may leave a partially written line.
Call `Logger::flush` explicitly for buffered output.

## Static string attributes

```rust
use slogr_prototype::{Attribute, attribute, info};

static ACCT_NAME: Attribute = attribute!("account.name", "Account name");
let name = "Ada";
let value = 42u32;
info!("hello {}, value {value:04x}", ACCT_NAME = name)?;
# Ok::<(), std::io::Error>(())
```

`ACCT_NAME = name` associates the next positional placeholder with a declared
attribute; it is not a named formatting argument or an assignment to the handle.
The handle supplies the string type, so `{}` or `{:s}` is accepted. The stored
placeholder is `{@account.name}` regardless of the Rust variable or import alias.
Canonical names cannot be empty, reserved (`log.relation`, `log.statement`), or
contain braces/colons, which would conflict with placeholder syntax.

Use `static` or `const` handles, not runtime locals. `attribute!` pre-encodes the
relation definition and canonical-name fragments. The earlier runtime
`register_attribute` API has been replaced rather than maintained as a second
metadata path. Names are process-wide, not namespaced; dots have no scoping
meaning. Equal declarations may appear in multiple modules. First use checks
cross-module conflicts before output; the compiler cannot check declarations
across independently compiled modules. The current API supports only strings.

## Statement templates and events

Every `info!` or `message!` callsite has a static anonymous `log.statement`
entity. Its complete JSON value is assembled during Rust constant evaluation:

```json
{
  "attrs": {},
  "refs": [{"account.name": {"$arg": 0}}],
  "msg": "hello {@account.name}, value {value:04x}"
}
```

The template contains no `args` or nested `defs`. `{"$arg": 0}` binds to the
first argument of each event using it; it is not a pointer into the template's
record. Ordinary JSON Pointer semantics are unchanged. Repeated associations
remain separate `refs` entries. Static severity will eventually go in template
`attrs`; nonempty template attributes are not supported yet.

First use assigns a process-local nonzero `u64` identity. `attrs.log.statement`
is a typed reference to that entity, resolved through `defs["log.statement:<id>"]`.
IDs are not globally qualified references and must not be conflated across
processes. Distinct callsites have distinct identities even for equal templates.
Declarations alone emit nothing. The first occurrence carries its statement,
attribute, and `log.statement` relation definitions in the same event. Later
occurrences contain only `attrs` and `args`. New callsites introduce their own
statement definitions but need not repeat already-emitted relations.

Each caller-owned logger tracks first use independently. Stdout tracks it per
thread alongside its scratch buffer, so different threads may repeat identical
definitions. Definitions are marked emitted only after the output accepts the
record. `message!` instead includes all required definitions in each owned
snapshot so that `Message::render()` works independently.

`replay` retains definitions and compiles each template once into formatting
operations and argument bindings; `Renderer` exposes the same behavior directly.
It binds each occurrence without copying an expanded record. Playback renders the message followed by an
association line (`account.name = Ada`), with multiple associations separated
by commas. The association line escapes control characters. Unknown relations,
conflicting definitions, type mismatches, and invalid bindings stop playback.
The prototype implements `$arg` bindings, not generic entity pointers yet.
The earlier per-record `msg`/`refs` wire format is no longer accepted.

## Producer allocation profile

The proc macro parses formats and pre-encodes immutable statement and relation
values at compile time. `info!` constructs a stack array of borrowed arguments and writes
JSON directly into reusable storage. It does not construct an owned `Message`,
copy strings into temporary allocations, or parse the format again at runtime.

Storage and rollback are adapted from the original direct OTLP implementation.
`Logger::new` preallocates 256 bytes and retains its buffer between records.
Larger records can grow it; once sufficient capacity and first-use definition
bookkeeping are available, successful recording performs zero library heap
allocations. First use validates the static schema and assigns the statement's
identity once; subsequent recording neither locks the registry nor re-encodes
definitions. By default stdout uses the
same approach with a per-thread scratch buffer, without global logger setup.
Encoding happens before acquiring the stdout lock.

Fixed stack storage avoids the initial heap-buffer allocation, but first-use
definition tracking can still allocate:

```rust
use slogr_prototype::{Logger, buffer::StackBuffer, info};

let mut logger = Logger::with_buffer(std::io::sink(), StackBuffer::<512>::new());
let name = "Ada";
let value = 42u32;
info!("hello {name:s}, value {value:04x}", logger: logger)?;
# Ok::<(), std::io::Error>(())
```

`HeapBuffer::with_capacity_and_limit` also permits an explicit record-size
limit. Limits count the complete encoded line, including JSON escaping and
the newline. An oversized record returns `StorageFull` without writing any
of it to the output, and the buffer can be reused. No positional arguments
are silently dropped and no JSON strings are byte-truncated.

Allocation tests cover warmed heap and stack storage and repeated attributed
statements with changing values. The
zero-allocation target excludes work done by user argument expressions and
output sinks, first-use schema validation/bookkeeping, buffer initialization/growth,
and error reporting. `message!`, validation of manually
constructed messages, playback, and graph-building may allocate. Throughput,
contention, and retained-memory policies still need production evaluation.

## Format subset

- `:s` captures an `AsRef<str>` value. Its stored placeholder omits `:s`.
- `{}` also captures strings when the positional argument uses a static
  attribute handle, such as `ACCT_NAME = name`; its stored label is `@account.name`.
- `:x` captures a `u8`, `u16`, `u32`, or `u64` value without formatting it first.
  `#` includes `0x`, `0` enables zero padding, and a constant width specifies
  the minimum width including any prefix. For example, `:04x` renders `002a`
  and `:#04x` renders `0x2a`. Width is limited to 4096.
- `{{` and `}}` encode literal braces. Argument data is never parsed as a format.
- Implicit captures name local variables; empty source placeholders consume
  explicit arguments. Unattributed labels come from source identifiers or
  expressions; attributed labels use canonical names. All placeholders consume
  `args` in occurrence order, not by name lookup.

Bare `{name}` string capture, signed integers, floats, `Debug`, named argument
overrides, and indexed placeholders are deliberately unsupported. Bind explicit
expressions containing braces or colons to local variables first, so their
stored labels cannot be mistaken for placeholder syntax. Invalid source formats,
wrong argument types, and missing or surplus arguments are compilation errors.
JSON retains the full `u64` range; the playback reader does not convert it to
floating point.

The small `slogr-message` crate shares the format grammar between the capture
proc macros (`slogr-prototype-macros`) and playback. `slogr-prototype` exposes
the public macros, caller-owned logger, and reader. These APIs are experimental.
