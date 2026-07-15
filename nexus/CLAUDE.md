# jaspy-nexus

Rust binary crate (Rocket + Diesel/Postgres) with an embedded React web UI
(`webui/`, built separately with Vite; `build.rs` writes a placeholder
`webui/dist/index.html` so plain `cargo build`/`cargo test` works without npm).

## Build & test

```sh
cargo build                     # normal build
cargo test --bin jaspy-nexus    # unit tests: fast, no external services
cargo test --test e2e           # e2e: needs postgres binaries + libpq/liboping
cargo test                      # both
```

On macOS (Homebrew), export the native-lib env first — see the "Running"
section of `tests/README.md` for the exact `PKG_CONFIG_PATH` /
`JASPY_TEST_PG_BINDIR` block.

## Testing policy (unit-first)

Every feature or fix that adds/changes logic must come with unit tests.
E2e tests (`tests/e2e.rs`) are reserved for (a) crucial core flows — ingest
contracts to Postgres, snmpbot query shapes, MQTT publications, trap wiring —
and (b) behavior unit tests cannot reach (startup, migrations, routing).
Full policy and rationale: `tests/README.md`.

Conventions:

- Unit tests are inline `#[cfg(test)] mod tests { use super::*; ... }` at the
  bottom of the module under test (this is a bin crate; inline modules can
  test private items — don't widen visibility for tests).
- If logic is tangled with I/O, extract a pure function and test that
  (examples: `traphandler::parse_trap_text`/`link_event_from_trap`,
  `poller::merge_query_result`).
- Reuse fixtures from `tests/fixtures/` via
  `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/..."))`.
- IMDS in tests: construct with `MessageBus::disconnected()` (test-only
  constructor in `utilities/msgbus.rs`); never depend on `JASPY_*` env vars.
- Don't read/write real env vars or other process-global state in unit tests;
  they run in parallel in one process.
- Metric assertions can be exact strings: `LabeledMetric::as_text()` sorts
  labels deterministically.
