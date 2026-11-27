# sprintr

**Structured PRINT in Rust**: an experiment in structured, formatted output with self-describing schemas
and in-log references. **Production-quality producer performance is a core
requirement of the experiment**, not an optimization postponed until later.
See [ARCHITECTURE.md](ARCHITECTURE.md) for the design and planned entity model.

The current milestone is a standalone JSON Lines logger and plain-text reader.
It pre-encodes statement templates and statically declared string attributes.
`info!`, `warn!`, and `error!` attach named levels to those static templates.
Call `start()` explicitly at application startup. Ordinary logging returns `()`;
before startup, it prints a warning and the event to stderr.
Logging calls deliver each event to the SDK as it is created. Events carry
a statement reference and typed arguments; first-use definitions
bootstrap the reader. Playback executes the stored formatting. It is not production-ready.
There is no Tokio, tracing, or OpenTelemetry SDK dependency.

## Read a recording

Install the `sprintr` command from the repository root:

```sh
cargo install --locked --path crates/sprintr-cli
```

Pipe your application's JSON Lines to `sprintr replay`, or pass a saved recording:

```sh
./your-app | sprintr replay
sprintr replay run.jsonl
sprintr replay --symbols run.jsonl
```

`--symbols` prints stacks using symbol packs embedded in the recording.
For recordings with a separate module map, use `--module-map modules.json`
to display linked addresses instead. Start at the beginning of the recording
so the reader sees any definitions that later events reuse.

## Compare the compiled telemetry schema

Applications can serialize `sprintr_prototype::schema::catalog()?` before
starting the logger. The DFE example exposes this as an export-and-exit flag:

```sh
cargo run -q -p sprintr-prototype --example dfe_hierarchy -- --print-telemetry-schema > head.json
sprintr schema diff base.json head.json > telemetry-diff.md
sprintr schema diff base.json head.json --format json
```

Generate both catalogs from the same binary target and feature set. The JSON
catalog includes resolved relation names, declaration origins, entity identity
rules, static named values, statement and `structured!` templates, and ordered
argument bindings and capture types. A dynamic `{}` slot stays dynamic; the
exporter never calls user formatters or samples runtime entity payloads.

The report compares fields rather than lines of JSON. Moving an unchanged
statement is silent; repeated statements retain their multiplicity. For an
edit in the same file and module, unique wording or a unique nonempty sequence
of argument bindings can identify the pair. Ambiguous changes appear as
removals and additions. Differences are informational (exit status zero);
invalid input and I/O failures are errors.

## Try it

```sh
cargo run --quiet -p sprintr-prototype --example json_lines -- record
```

After the startup announcement:

```json
{"seq":1,"stmt":2,"thread":88888,"ts":1704067200123456789,"attrs":{"log.stack":[4336737584,4336770768]},"args":["Ada",42],"defs":{"log.relation:account.name":{"kind":"attribute","type":"string","desc":"Account name"},"log.statement:2":{"attrs":{"log.level":"INFO"},"refs":[{"account.name":{"$arg":0}}],"msg":"hello {@account.name}, value {value:04x}","source":{"mod":"json_lines","file":"crates/sprintr-prototype/examples/json_lines.rs","line":25,"column":17}}}}
{"seq":2,"stmt":2,"thread":88888,"ts":1704067200124567890,"attrs":{"log.stack":[4336737584,4336770768]},"args":["Grace",42]}
```

```sh
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

See the [prototype API and format subset](crates/sprintr-prototype/README.md)
for [nested structured arguments](crates/sprintr-prototype/README.md#structured-arguments),
anonymous and [scoped named entities](crates/sprintr-prototype/README.md#globally-and-locally-named-entities),
[scoped entity context](crates/sprintr-prototype/README.md#scoped-entity-context),
implicit captures, positional expressions,
caller-owned writers, and [deferred call-stack symbolization](crates/sprintr-prototype/README.md#call-stacks-and-symbols).
The [DFE hierarchy example](crates/sprintr-prototype/examples/dfe_hierarchy.rs)
shows two batches traced through an engine, group, pipeline, and node.
`crates/sprintr-prototype/examples/exception_chain.rs` shows a Rust error
enum wrapping a structured cause, a warning that refers to it, and
`Caused by:` playback in a separate process.

## Workspace

| Crate | Purpose |
|-------|---------|
| `sprintr-cli` | Installable `sprintr` command for reading recordings |
| `sprintr-message` | Message representation, shared format grammar, and rendering |
| `sprintr-prototype-macros` | Compile-time format parsing and typed argument capture |
| `sprintr-prototype` | Public macros, direct JSON recording, reusable buffers, and playback |

Additional attribute types, graph navigation, and binary
encoding are future work. The old tracing/OTLP runtime, adapters, and examples have been
removed rather than maintained as a second implementation.

## Performance

`info!` uses compile-time-encoded statement templates and a stack array of borrowed
arguments. It writes directly into reusable heap or fixed stack storage,
without constructing an owned message or reparsing the format on the producer
path. Successful steady-state recording makes zero library heap allocations
once storage and definition bookkeeping are initialized. Repeated events emit
only `seq`, `stmt`, `thread`, `ts`, the event stack in `attrs`, and `args`, without re-encoding templates or taking
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
