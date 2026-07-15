# jaspy-nexus tests

There are two suites:

- **Unit tests** — inline `#[cfg(test)] mod tests` modules in `src/`, run with
  `cargo test --bin jaspy-nexus`. No PostgreSQL, MQTT, or network needed;
  they finish in well under a second. This is where most coverage lives.
- **End-to-end tests** — `tests/e2e.rs`, run with `cargo test --test e2e`.
  Boots the real binary against ephemeral Postgres + mock snmpbot + embedded
  MQTT (see below). Slow but exercises the full process.

Plain `cargo test` runs both.

## Testing policy

**Unit-first.** New logic gets unit tests in its own module, written against
the pure core: parsing, table/entry transforms, state machines, metric
rendering. If the logic you want to test is tangled with I/O (HTTP, DB, MQTT),
prefer extracting a pure function and unit-testing that over booting the e2e
harness — see `traphandler::parse_trap_text` / `link_event_from_trap` and
`poller::merge_query_result` for examples of exactly this split.

**E2e is reserved for** (a) crucial core flows — the HTTP↔DB ingest contracts,
snmpbot query shapes on the wire, MQTT event publication, trap ingest wiring,
discovery crawling end to end — and (b) behavior that unit tests cannot reach
(process startup, migrations, Rocket routing/guards, config/env plumbing).
Don't add an e2e test for logic a unit test can cover.

Unit test conventions:

- Inline `#[cfg(test)] mod tests { use super::*; ... }` at the bottom of the
  module under test; this gives access to private functions — don't widen
  visibility for tests.
- Reuse the snmpbot/trap fixtures via
  `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/..."))`.
- Build an IMDS with `MessageBus::disconnected()` (test-only constructor).
- Never read or write real environment variables in unit tests — tests run in
  parallel in one process.

# End-to-end tests

`tests/e2e.rs` boots the **real** `jaspy-nexus` binary against a **mock snmpbot**
(`httpmock`), an **ephemeral PostgreSQL** (spun up per test via `initdb`/`pg_ctl`),
and an **embedded MQTT broker** (`rumqttd`), then asserts the externally
observable contracts:

1. nexus issues the correct SNMP queries to snmpbot (right tables + `community@fqdn`, nothing extra);
2. the ingest endpoints write the correct rows to Postgres;
3. the correct Prometheus metrics are exposed at `/dev/metrics` and `/dev/metrics/fast`
   (including the entity sensor and per-VLAN STP metrics from the in-process
   entitypoller collector);
4. MQTT events are published on device changes.

## Prerequisites on the test host

- **PostgreSQL client/server binaries** — `initdb`, `pg_ctl`, `createdb`. The
  harness finds them on `PATH`, or via `JASPY_TEST_PG_BINDIR`.
- **liboping** and **libpq** — the `jaspy-nexus` binary links them at runtime.
- `pkg-config` for building (`oping`/`pq-sys` build scripts).

Each test creates its own throwaway Postgres cluster in a `tempdir` (TCP on a
free `127.0.0.1` port, socket dir `/tmp`) and tears it down on drop. No shared
state; tests are isolated.

## Running

```sh
cargo test --test e2e
```

On macOS (Homebrew), set the native-lib env first — the repo ships a helper that
does this (`scratchpad` `test.sh`), equivalent to:

```sh
export PATH="/opt/homebrew/opt/postgresql@17/bin:/opt/homebrew/bin:$PATH"
export PKG_CONFIG_PATH="/opt/homebrew/opt/liboping/lib/pkgconfig:/opt/homebrew/opt/libpq/lib/pkgconfig"
export LIBRARY_PATH="/opt/homebrew/opt/liboping/lib:/opt/homebrew/opt/libpq/lib"
export DYLD_FALLBACK_LIBRARY_PATH="/opt/homebrew/opt/liboping/lib:/opt/homebrew/opt/libpq/lib:/usr/lib"
export JASPY_TEST_PG_BINDIR="/opt/homebrew/opt/postgresql@17/bin"
cargo test --test e2e
```

On Linux/CI these libraries live in standard paths; just ensure the postgres
binaries and `liboping`/`libpq` (+ `-dev`) packages are installed.

## Notes

- The suite relies on env knobs the binary exposes for fast, deterministic runs:
  `POLL_LOOP_MSECS`, `JASPY_IMDS_REFRESH_SECS`, `JASPY_POLLER_NO_JITTER`,
  `JASPY_ENABLE_POLLER/PINGER`, `JASPY_SNMPBOT_URL`, `JASPY_MQTT_SERVER`
  (`host:port`), `JASPY_DB_URL`, `ROCKET_ADDRESS/PORT`.
- The **pinger** (ICMP) is not exercised (raw sockets need privilege);
  `jaspy_device_up` is covered via the `PUT /dev/device/monitor` ingest path.
- `entitypoller` is a separate service and out of scope.
- Fixtures in `tests/fixtures/` are snmpbot table responses; edit them to change
  expected metric values.
