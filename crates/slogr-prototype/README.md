# Standalone slogr prototype

This is the first, non-production milestone from `ARCHITECTURE.md`: capture
a message format and typed positional arguments, record JSON Lines, and
replay plain-text messages. There are no attributes, entities, references,
bootstrap records, severity fields, filtering, or binary encoding yet.
There is no dependency on Tokio, tracing, or an OpenTelemetry runtime.

## Run the example

From the repository root:

```sh
cargo run --quiet -p slogr-prototype --example json_lines -- record
```

```json
{"msg":"hello {name}, value {value:04x}","args":["Ada",42]}
```

Replay from stdin:

```sh
cargo run --quiet -p slogr-prototype --example json_lines -- record \
  | cargo run --quiet -p slogr-prototype --example json_lines -- replay
```

```text
hello Ada, value 002a
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

## Producer allocation profile

The proc macro parses the format and JSON-escapes the static record prefix at
compile time. `info!` constructs a stack array of borrowed arguments and writes
JSON directly into reusable storage. It does not construct an owned `Message`,
copy strings into temporary allocations, or parse the format again at runtime.

Storage and rollback are adapted from the original direct OTLP implementation.
`Logger::new` preallocates 256 bytes and retains its buffer between records.
Larger records can grow it; once sufficient capacity is available, successful
recording performs zero library heap allocations. By default stdout uses the
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

Allocation tests cover warmed heap storage and stack storage from the first
record, including string escaping and integers. The zero-allocation target
excludes work done by user argument expressions and output sinks, buffer
initialization/growth, and error reporting. `message!`, validation of manually
constructed messages, playback, and graph-building may allocate. Throughput,
contention, and retained-memory policies still need production evaluation.

## Format subset

- `:s` captures an `AsRef<str>` value. Its stored placeholder omits `:s`.
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
