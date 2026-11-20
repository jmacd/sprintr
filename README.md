# slogr

An experiment in structured, formatted logging with self-describing schemas
and in-log references. **Production-quality producer performance is a core
requirement of the experiment**, not an optimization postponed until later.
See [ARCHITECTURE.md](ARCHITECTURE.md) for the design and planned entity model.

The current milestone is a standalone JSON Lines logger and plain-text reader.
It captures source-derived message formats, typed arguments, and registered
string attributes, then executes the recorded formatting during playback.
Attribute definitions bootstrap the reader on first use. It is not production-ready.
There is no Tokio, tracing, or OpenTelemetry SDK dependency.

## Try it

```sh
cargo run --quiet -p slogr-prototype --example json_lines -- record
```

```json
{"msg":"hello {acct_name}, value {value:04x}","args":["Ada",42],"defs":{"log.relation:account.name":{"kind":"attribute","type":"string","desc":"Account name"}},"refs":[{"account.name":"#/args/0"}]}
```

```sh
cargo run --quiet -p slogr-prototype --example json_lines -- record \
  | cargo run --quiet -p slogr-prototype --example json_lines -- replay
```

```text
hello Ada, value 002a
account.name = Ada
```

See the [prototype API and format subset](crates/slogr-prototype/README.md)
for implicit captures, positional expressions, caller-owned writers, and
stack-backed encoding.

## Workspace

| Crate | Purpose |
|-------|---------|
| `slogr-message` | Message representation, shared format grammar, and rendering |
| `slogr-prototype-macros` | Compile-time format parsing and typed argument capture |
| `slogr-prototype` | Public macros, direct JSON recording, reusable buffers, and playback |

Generic entities, additional attribute types, graph navigation, and binary
encoding are future work. The old tracing/OTLP runtime, adapters, and examples have been
removed rather than maintained as a second implementation.

## Performance

`info!` uses a compile-time-encoded JSON prefix and a stack array of borrowed
arguments. It writes directly into reusable heap or fixed stack storage,
without constructing an owned message or reparsing the format on the producer
path. Successful steady-state recording makes zero library heap allocations
once storage and definition bookkeeping are initialized. Plain messages need
no initial heap allocation with sufficient stack storage. Attribute registration
caches definition bytes; repeated uses emit only positional `refs`, with no
registry lock. Caller expressions, output sinks, initialization/growth, and
error reporting are outside that allocation budget.

The planned entity API must capture and encode a snapshot once, then emit
references on repeated use. JSON versus binary encoding is secondary to
preserving direct encoding, reuse, and low per-event overhead. Allocation
counts, throughput, latency, contention, and retained memory all matter.
The current allocation assertions do not establish production readiness.

## Schema tooling

The `tools/weaver` submodule is retained for future schema work. It is not a
workspace member or a build dependency of this milestone. The old `slogr-build`
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
