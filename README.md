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

```json
{"attrs":{"log.statement":1},"args":["Ada",42],"defs":{"log.relation:log.statement":{"kind":"entity","type":"object","desc":"Logging statement","anonymous":true},"log.relation:log.level":{"kind":"entity","type":"object","desc":"Logging level"},"log.level:INFO":{"desc":"Informational event"},"log.relation:account.name":{"kind":"attribute","type":"string","desc":"Account name"},"log.statement:1":{"attrs":{"log.level":"INFO"},"refs":[{"account.name":{"$arg":0}}],"msg":"hello {@account.name}, value {value:04x}"}}}
{"attrs":{"log.statement":1},"args":["Grace",42]}
```

```sh
cargo run --quiet -p sprintr-prototype --example json_lines -- record \
  | cargo run --quiet -p sprintr-prototype --example json_lines -- replay
```

```text
hello Ada, value 002a
log.level = INFO, account.name = Ada
hello Grace, value 002a
log.level = INFO, account.name = Grace
```

See the [prototype API and format subset](crates/sprintr-prototype/README.md)
for implicit captures, positional expressions, caller-owned writers, and
stack-backed encoding.

## Workspace

| Crate | Purpose |
|-------|---------|
| `sprintr-message` | Message representation, shared format grammar, and rendering |
| `sprintr-prototype-macros` | Compile-time format parsing and typed argument capture |
| `sprintr-prototype` | Public macros, direct JSON recording, reusable buffers, and playback |

Generic entities, additional attribute types, graph navigation, and binary
encoding are future work. The old tracing/OTLP runtime, adapters, and examples have been
removed rather than maintained as a second implementation.

## Performance

`info!` uses compile-time-encoded statement templates and a stack array of borrowed
arguments. It writes directly into reusable heap or fixed stack storage,
without constructing an owned message or reparsing the format on the producer
path. Successful steady-state recording makes zero library heap allocations
once storage and definition bookkeeping are initialized. Repeated events emit
only `attrs.log.statement` and `args`, without re-encoding templates or taking
the schema registry lock. First use may allocate even with stack-backed storage.
Caller expressions, output sinks, initialization/growth, and
error reporting are outside that allocation budget.

The planned entity API must capture and encode a snapshot once, then emit
references on repeated use. JSON versus binary encoding is secondary to
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
