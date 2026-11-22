# sprintr

**Structured PRINT in Rust**: an experiment in structured, formatted output with self-describing schemas
and in-log references. **Production-quality producer performance is a core
requirement of the experiment**, not an optimization postponed until later.
See [ARCHITECTURE.md](ARCHITECTURE.md) for the design and planned entity model.

The current milestone is a standalone JSON Lines logger and plain-text reader.
It pre-encodes statement templates and statically declared string attributes.
`info!`, `warn!`, and `error!` attach named levels to those static templates.
Events carry a statement reference and typed arguments; first-use definitions
bootstrap the reader. Playback executes the stored formatting. It is not production-ready.
There is no Tokio, tracing, or OpenTelemetry SDK dependency.

## Try it

```sh
cargo run --quiet -p sprintr-prototype --example json_lines -- record
```

After the startup announcement:

```json
{"seq":1,"stmt":2,"thread":88888,"args":["Ada",42],"defs":{"log.relation:account.name":{"kind":"attribute","type":"string","desc":"Account name"},"log.statement:2":{"attrs":{"log.level":"INFO"},"refs":[{"account.name":{"$arg":0}}],"msg":"hello {@account.name}, value {value:04x}"}}}
{"seq":2,"stmt":2,"thread":88888,"args":["Grace",42]}
```

```sh
cargo run --quiet -p sprintr-prototype --example json_lines -- record \
  | cargo run --quiet -p sprintr-prototype --example json_lines -- replay
```

```text
process 50316 started logging with sprintr 0.1.0
log.level = INFO, process.pid = 50316, telemetry.sdk.name = sprintr, telemetry.sdk.version = 0.1.0
hello Ada, value 002a
log.level = INFO, account.name = Ada
hello Grace, value 002a
log.level = INFO, account.name = Grace
```

See the [prototype API and format subset](crates/sprintr-prototype/README.md)
for anonymous application objects, implicit captures, positional expressions,
caller-owned writers, and stack-backed encoding.

## Workspace

| Crate | Purpose |
|-------|---------|
| `sprintr-message` | Message representation, shared format grammar, and rendering |
| `sprintr-prototype-macros` | Compile-time format parsing and typed argument capture |
| `sprintr-prototype` | Public macros, direct JSON recording, reusable buffers, and playback |

Runtime named entities, additional attribute types, graph navigation, and binary
encoding are future work. The old tracing/OTLP runtime, adapters, and examples have been
removed rather than maintained as a second implementation.

## Performance

`info!` uses compile-time-encoded statement templates and a stack array of borrowed
arguments. It writes directly into reusable heap or fixed stack storage,
without constructing an owned message or reparsing the format on the producer
path. Successful steady-state recording makes zero library heap allocations
once storage and definition bookkeeping are initialized. Repeated events emit
only `seq`, `stmt`, `thread` and `args`, without re-encoding templates or taking
the schema registry lock. First use may allocate even with stack-backed storage.
Caller expressions, output sinks, initialization/growth, and
error reporting are outside that allocation budget.

The anonymous entity API captures a snapshot once, then emits references on
repeated use. JSON versus binary encoding is secondary to
preserving direct encoding, reuse, and low per-event overhead. Allocation
counts, throughput, latency, contention, and retained memory all matter.
The current allocation assertions do not establish production readiness.

## Schema tooling

The `tools/weaver` submodule is retained for future schema work. It is not a
workspace member or a build dependency of this milestone. The old `sprintr-build`
helper and its example targeted the removed runtime and have been removed.

## Development

```sh
cargo test --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Tests cover recording/playback, compile-time API errors, malformed input,
I/O failures, buffer rollback, and producer allocation budgets. Rust source
is ASCII-only. Document tests with `Scenario:` and `Guarantees:` comments.

## Provenance and license

The heap/stack storage and rollback implementation is adapted from the original
OpenTelemetry Arrow extraction. Its Apache-2.0 notices are retained; see
`LICENSE` and `NOTICE`. The obsolete Tokio-derived macro implementation has
been removed.
