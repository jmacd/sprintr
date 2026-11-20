# Standalone slogr prototype

This non-production prototype captures a message format and typed positional
arguments, records JSON Lines, and replays plain-text messages. Registered
string attributes add self-describing definitions and positional references.
There are no generic entities, severity fields, filtering, or binary encoding yet.
There is no dependency on Tokio, tracing, or an OpenTelemetry runtime.

## Run the example

From the repository root:

```sh
cargo run --quiet -p slogr-prototype --example json_lines -- record
```

```json
{"msg":"hello {acct_name}, value {value:04x}","args":["Ada",42],"defs":{"log.relation:account.name":{"kind":"attribute","type":"string","desc":"Account name"}},"refs":[{"account.name":"#/args/0"}]}
```

Replay from stdin:

```sh
cargo run --quiet -p slogr-prototype --example json_lines -- record \
  | cargo run --quiet -p slogr-prototype --example json_lines -- replay
```

```text
hello Ada, value 002a
account.name = Ada
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
Writers are synchronous, and I/O failure may leave a partially written line.
Call `Logger::flush` explicitly for buffered output.

## Registered string attributes

```rust
use slogr_prototype::{info, register_attribute};

let acct_name = register_attribute("account.name", "Account name")?;
let name = "Ada";
let value = 42u32;
info!("hello {}, value {value:04x}", acct_name = name)?;
# Ok::<(), std::io::Error>(())
```

`acct_name = name` associates the next positional placeholder with a registered
attribute; it is not a named formatting argument or an assignment to the handle.
The handle supplies the string type, so `{}` or `{:s}` is accepted. Its local
Rust alias labels the stored placeholder; the canonical name `account.name`
appears in `defs`, `refs`, and the rendered association line.

Registration is process-wide. Identical name/type/description definitions reuse
the same handle data; conflicting definitions fail. Names are not namespaced,
and dots have no scoping meaning. The initial API supports only strings.

Registration emits nothing. On first use, the same data record contains the
`log.relation:account.name` definition and `refs: [{"account.name":"#/args/0"}]`.
Later records from that logger omit the definition but retain their `refs`.
Repeated associations with one name remain separate list entries. There is no
`attrs` section: event-level labels such as severity are future work.

Each caller-owned logger tracks first use independently. Stdout tracks it per
thread alongside its scratch buffer, so different threads may repeat identical
definitions. Definitions are marked emitted only after the output accepts the
record. `message!` instead includes all required definitions in each owned
snapshot so that `Message::render()` works independently.

`replay` retains definitions between records; `Renderer` provides the same
stateful behavior directly. Playback renders the message followed by an
association line (`account.name = Ada`), with multiple associations separated
by commas. The association line escapes control characters. Unknown relations,
conflicting definitions, type mismatches, and invalid pointers stop playback.
This extension supports only local `#/args/<index>` references.

## Producer allocation profile

The proc macro parses the format and JSON-escapes the static record prefix at
compile time. `info!` constructs a stack array of borrowed arguments and writes
JSON directly into reusable storage. It does not construct an owned `Message`,
copy strings into temporary allocations, or parse the format again at runtime.

Storage and rollback are adapted from the original direct OTLP implementation.
`Logger::new` preallocates 256 bytes and retains its buffer between records.
Larger records can grow it; once sufficient capacity and first-use definition
bookkeeping are available, successful recording performs zero library heap
allocations. Registration pre-encodes relation definitions and reference prefixes;
the recording path neither locks the registry nor re-encodes definitions.
By default stdout uses the
same approach with a per-thread scratch buffer, without global logger setup.
Encoding happens before acquiring the stdout lock.

Fixed stack storage avoids even the initial heap-buffer allocation:

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

Allocation tests cover warmed heap storage, plain messages in stack storage
from the first record, and repeated registered-attribute recording. The
zero-allocation target excludes work done by user argument expressions and
output sinks, registration, first-use bookkeeping, buffer initialization/growth,
and error reporting. `message!`, validation of manually
constructed messages, playback, and graph-building may allocate. Throughput,
contention, and retained-memory policies still need production evaluation.

## Format subset

- `:s` captures an `AsRef<str>` value. Its stored placeholder omits `:s`.
- `{}` also captures strings when the positional argument uses a registered
  attribute handle, such as `acct_name = name`.
- `:x` captures a `u8`, `u16`, `u32`, or `u64` value without formatting it first.
  `#` includes `0x`, `0` enables zero padding, and a constant width specifies
  the minimum width including any prefix. For example, `:04x` renders `002a`
  and `:#04x` renders `0x2a`. Width is limited to 4096.
- `{{` and `}}` encode literal braces. Argument data is never parsed as a format.
- Implicit captures name local variables; empty source placeholders consume
  explicit arguments. Stored labels come from source identifiers or expressions
  and consume the `args` array in occurrence order, not by name lookup.

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
