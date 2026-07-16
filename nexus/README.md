# jaspy-nexus

Implementation of Jaspy2 API and related components

## Dependencies

- Rust
- `libpq`
    - Debian(s): `apt install libpq-dev`
    - Gentoo: `emerge -av dev-libs/libpqxx`
    - Fedora: `dnf install libpq-devel`
- SQLite is bundled (compiled into the binary) — no system library needed.

## Database backends

`JASPY_DB_URL` selects the backend at runtime:

| URL form | Backend |
|---|---|
| `postgres://user:pass@host/db` (or `postgresql://`) | PostgreSQL |
| `sqlite:///var/lib/jaspy/jaspy.db`, `sqlite:jaspy.db`, or a bare path | SQLite |

Pending schema migrations for the active backend are applied automatically at
startup; set `JASPY_AUTO_MIGRATE=false` if your deployment manages schema
externally (e.g. with the diesel CLI). The Maintenance page shows the
configured backend, live connectivity and whether migrations are pending.

SQLite notes: intended for small deployments and local development. The
database runs in WAL mode (expect `-wal`/`-shm` sidecar files next to the
database file); writes are serialized by SQLite's single-writer lock, which is
ample for small networks but makes PostgreSQL the right choice for large ones.
SQLite schema lives in `migrations_sqlite/` — schema changes must be added
pairwise with `migrations/`.

## Mock mode — local development against a fake network

`jaspy-nexus mock` starts the full application (HTTP API, web UI, Prometheus
metrics, WebSocket event streams) against a small built-in fake network, so
you can develop apps that use the jaspy API without snmpbot, real devices, or
any configuration:

```sh
# build the web UI once (optional but recommended; see webui/)
(cd webui && npm ci && npm run build)

cargo run -- mock
```

There are **no prerequisites**: mock mode uses a throwaway SQLite database in
a temp dir (removed on Ctrl-C). To exercise the PostgreSQL path instead, set
`JASPY_MOCK_PG=1` to spawn a throwaway postgres (needs `initdb`/`pg_ctl`/
`createdb` on PATH or `JASPY_PG_BINDIR`), or point `JASPY_DB_URL` at any
existing database of either backend; migrations run automatically.

What you get: an in-process snmpbot-compatible server with 8 fake devices
(core, 2 distribution, 3 access switches, a WLC and a firewall) crawled by the
**real** discovery engine and polled by the **real** collectors — so the data
took the same code paths it takes in production. The network is alive:
counters grow, sensor temperatures drift, and the `access-hall-a-02` uplink
flaps every 60 s, producing live interface up/down events. LACP port-channels
are represented too: healthy 2×10G bundles between the core and each dist
switch (both ends monitored, exercising the far-end cross-checks), a healthy
server bundle on `access-hall-a-01`, and a deliberately misconfigured bundle
on `access-hall-a-02` that trips the port-channel warnings.

| What | Where |
|---|---|
| Web UI | http://127.0.0.1:8000/ |
| Devices API | http://127.0.0.1:8000/api/v1/devices |
| Summary | http://127.0.0.1:8000/api/v1/summary |
| Per-device sensors/STP | http://127.0.0.1:8000/api/v1/devices/core1.mock.jaspy/entity |
| Prometheus metrics | http://127.0.0.1:8000/dev/metrics |
| Live discovery log (WS) | ws://127.0.0.1:8000/api/v1/ws/logs/discovery |
| Live device events (WS) | ws://127.0.0.1:8000/api/v1/ws/logs/device:access-hall-a-02.mock.jaspy |
| Fake snmpbot | http://127.0.0.1:18286/api/hosts/core1.mock.jaspy/tables/IF-MIB::ifTable |

All 8 devices appear within ~15 s (first discovery crawl); client locations
and the event name are seeded shortly after. Frontend development: run
`npm run dev` in `webui/` — the Vite proxy targets port 8000, which is exactly
where mock mode listens.

Every setting is overridable: mock mode only fills in env vars you have not
set (`JASPY_POLL_LOOP_MSECS`, `JASPY_MOCK_SNMPBOT_PORT`, `ROCKET_PORT`, ...).
Caveat for the `JASPY_DB_URL` path: a previously persisted discovery
configuration in the `settings` table overrides the mock defaults.

## Missing features

 - Logging
