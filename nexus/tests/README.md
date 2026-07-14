# jaspy-nexus end-to-end tests

`tests/e2e.rs` boots the **real** `jaspy-nexus` binary against a **mock snmpbot**
(`httpmock`), an **ephemeral PostgreSQL** (spun up per test via `initdb`/`pg_ctl`),
and an **embedded MQTT broker** (`rumqttd`), then asserts the externally
observable contracts:

1. nexus issues the correct SNMP queries to snmpbot (right tables + `community@fqdn`, nothing extra);
2. the ingest endpoints write the correct rows to Postgres;
3. the correct Prometheus metrics are exposed at `/dev/metrics` and `/dev/metrics/fast`;
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
