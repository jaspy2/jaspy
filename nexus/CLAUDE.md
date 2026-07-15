# jaspy-nexus

Rust binary crate (Rocket + Diesel; PostgreSQL or SQLite selected at runtime
by the `JASPY_DB_URL` scheme) with an embedded React web UI
(`webui/`, built separately with Vite; `build.rs` writes a placeholder
`webui/dist/index.html` so plain `cargo build`/`cargo test` works without npm).

## Build & test

```sh
cargo build                     # normal build
cargo test --bin jaspy-nexus    # unit tests: fast, no external services
cargo test --test e2e           # e2e matrix: every test runs on pg AND sqlite
cargo test --test e2e -- ::sqlite   # sqlite side only (no postgres binaries needed)
cargo test                      # both
```

On macOS (Homebrew), export the native-lib env first — see the "Running"
section of `tests/README.md` for the exact `PKG_CONFIG_PATH` /
`JASPY_TEST_PG_BINDIR` block.

## Database layer

`src/db.rs` owns the backend abstraction: `AnyConnection`
(`#[derive(MultiConnection)]`, Sqlite variant last), a kind-matched r2d2
manager, sqlite WAL/busy_timeout pragmas, embedded migrations for both
backends (`migrations/` pg, `migrations_sqlite/` — schema changes go in
BOTH, pairwise), and startup auto-migration. Model queries take
`&mut db::AnyConnection`; upserts and INSERT..RETURNING are not expressible
through the enum — wrap those in the `with_backend!` macro (see
`Setting::set`). dbo unit tests run real CRUD against in-memory sqlite
(`AnyConnection::Sqlite(SqliteConnection::establish(":memory:")?)` + one
direct connection, never a pool) and double as the sqlite-migration drift
guard.

## Mock mode

`cargo run -- mock` starts the app against a built-in fake network (see
"Mock mode" in README.md). `src/mock/` is only active via that subcommand:
`topology.rs` (fake network, pure time-derived values), `snmpbot.rs`
(snmpbot-compatible TcpListener server), `pg.rs` (ephemeral postgres),
`seed.rs` (client locations + event name). The table generators must mirror
the snmpbot shapes in `tests/fixtures/` — they serialize through the same
`SNMPBotResponse` structs the collectors deserialize. Unit-first testing
applies to all of it; `mock_mode_serves_network` in tests/e2e.rs covers the
end-to-end crawl.

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
