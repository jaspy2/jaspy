# nexus SNMP polling — scalability notes

Baseline to preserve: **250 switches / ~4,000 active interfaces / 10 s poll
cycle**. This was achievable with the pre-consolidation standalone poller.
This document records where the current in-process design spends its time,
which parts are genuine regressions vs. long-standing bottlenecks, and how to
measure it (`perf/`).

## Architecture

All SNMP consumers now run inside one process (`nexus`) and share two pieces of
infrastructure that decide scalability:

1. **One `Arc<SnmpSource>`** — either the snmpbot HTTP sidecar
   (`snmp/snmpbot_http.rs`) or the embedded snmp2 v2c client (`snmp/embedded.rs`).
2. **One `Arc<Mutex<IMDS>>`** — every poll report and every Prometheus scrape
   contends on it.

| Collector | Threading | Interval | Default |
|---|---|---|---|
| `poller` (interface counters — hot path) | thread-per-device (~250 threads) | 10 s | on |
| `pinger` | thread-per-device | — | on |
| `entitypoller` (sensors + STP) | bounded, 16 workers | 120 s | on |
| `vlanpoller` | bounded, 16 workers | 300 s | on |
| `lagpoller` (port-channel/LACP) | bounded, 16 workers | 300 s | on |

## Findings (severity-ranked)

### 1. 🔴 O(N²) clone in the hot path, under the global lock  — ✅ FIXED (see results below)
`utilities/imds.rs` — in `report_interfaces`, `let mut interfaces_shadow =
device.interfaces.clone();` runs **once per interface** in the report loop, so a
device with N interfaces clones its whole interface map N times per poll. The
shadow is only read inside the rare up/down-transition branch. All of it happens
while holding the single `Mutex<IMDS>`.
- 48-port switch → ~2,300 interface-struct copies/device/10 s; ×250 ≈ 576k/cycle,
  serialized on one mutex. A high-density chassis is far worse.
- **Fix:** hoist the clone out of the loop; build the shadow only inside the
  `if old_state != new_state` branch. Near-zero risk, highest value.

### 2. 🔴 One global `Mutex<IMDS>` serializes all pollers + the scrape  — ✅ FIXED (see results below)
`poller.rs` holds the lock for the whole `report_interfaces` call; `/dev/metrics`
(`routes/dev/metrics.rs`) holds the *same* lock while `get_metrics` builds the
entire `Vec<LabeledMetric>` (~4,000 interfaces × ~7 metrics, two `labels.clone()`
each). During a scrape every poller blocks; during a heavy report the scrape and
all other pollers block.
- **Fix:** snapshot under the lock and build/format metrics outside it; or
  `RwLock`; or shard IMDS per-device.

### 3. 🔴 Embedded client issues ~N_columns× more round-trips per table  — ✅ FIXED (see results below)
`snmp/embedded.rs::build_table` walks **each column independently** with GETBULK.
ifTable (~22 cols) + ifXTable (~18) ≈ 40 independent walk sequences per device,
vs snmpbot's lockstep multi-column walk (all columns per row batch in one PDU).
Roughly an order of magnitude more UDP round-trips per device. Opt-in
(`snmp_mode=embedded`), but not a peer of snmpbot at this scale.
- **Fix:** multi-varbind GETBULK (request several column OIDs per PDU).

### 4. 🟠 Three new default-on collectors share the one snmpbot  — ✅ MECHANISM ADDED (see results below)
`main.rs` — `entitypoller`/`vlanpoller`/`lagpoller` (each `unwrap_or(true)`) add
up to 16 workers each against the sidecar the 250-thread poller already loads.
No global in-flight budget; jitter is `interval/2`, so the 120 s/300 s cycles
re-cluster over time. Most likely cause of a regression from the baseline.
- **Fix:** shared semaphore capping total in-flight requests to snmpbot; stagger
  collector phases.

### 5. 🟠 A fresh reqwest blocking client per SNMP request (no keep-alive)  — ✅ FIXED (see results below)
`snmp/snmpbot_http.rs::client()` builds a new `reqwest::blocking::Client` (own
tokio runtime, fresh TCP, no pool) on **every** table/object call. Thousands of
runtime-spawn + TCP-setup/teardown cycles per 10 s. Preserved from the old
`reqwest::blocking::get`, but a real ceiling.
- **Fix:** one reused `Client` (or one per worker thread) with keep-alive.

### 6. 🟡 Embedded opens a new UDP socket/session per table and per object  — ✅ FIXED (see results below)
`snmp/embedded.rs::table`/`object` call `SnmpSession::open` each time. Cache a
session per (host, cycle).

### 7. 🟡 Poller supervisor reloads all monitored devices from the DB every 1 s  — ✅ FIXED (see results below)
`poller.rs::run` calls `load_devices` every 1,000 ms just to detect add/remove.
Widen to 10–30 s.

## Threading assessment

Thread-per-device (~500 poll+ping threads) is not itself the bottleneck — the
threads are almost always blocked on I/O, and this is the model that hit the
baseline. The bottleneck is what they contend on: the single IMDS mutex (#1, #2)
and the single SNMP back end (#3, #4, #5). `run_bounded` (`collectors/pool.rs`)
is sound.

## Measuring it — `perf/`

`perf/` drives real SNMP UDP traffic from nexus against a simulated switch fleet
on loopback, so the client-side hot path (embedded walks, thread-per-device,
IMDS contention, metrics build) is exercised end to end. See `perf/README.md`.

In-process instrumentation (lock-free atomics, negligible overhead) is exposed at
`GET /dev/metrics/perf` (`utilities/perfstats.rs`), covering: device-poll
throughput and overruns, SNMP query count/errors/latency, **IMDS lock wait**
(the #2 signal), IMDS report hold time, and metrics-build time under the lock.

**Method:** capture a baseline with today's code *before* optimizing, then
re-run after each fix and compare the same counters. Target: all N devices
complete their poll within the 10 s cycle (`poll_overruns` ≈ 0) at 250/4,000.

## Baseline results (before optimization)

Measured with `perf/run.py` in embedded mode against the loopback fleet
(Apple Silicon, loopback SNMP so RTT is sub-ms — absolute numbers are
optimistic vs a real network, but the *trends* between rows isolate each
finding). All rows sustained 100% of target polls/s with 0 overruns; the
limits show as trends, not failures, because the cycle budget is generous and
the machine is fast.

| Config | interfaces | polls/s (target) | report hold (#1) | metrics build / max lock-wait (#2) | SNMP lat mean (#3) | scrape |
|---|---|---|---|---|---|---|
| Baseline 250×16 @10s | 4,000 | 25.0 (100%) | 0.03 ms | 18.5 ms / 15.7 ms | 1.5 ms | 15 s |
| Stress A 250×16 @1.5s | 4,000 | 166 (100%) | 0.03 ms | 31.3 ms / 28.4 ms (**mean 0.30 ms**) | 1.7 ms | 1 s |
| Stress B 250×128 @10s | 32,000 | 25.0 (100%) | **1.22 ms** | **141.9 ms / 121.5 ms** | 4.8 ms | 3 s |

What each row demonstrates:
- **#1 O(N²) clone** — report hold went 0.03 → 1.22 ms as interfaces/device
  went 16 → 128 (8×), i.e. ~40× — super-linear, the O(N²) signature. Trivial at
  16 ports, real on high-density chassis.
- **#2 single global mutex** — building the metric Vec under the lock took
  141.9 ms for a 56 MB / 32k-interface payload, and a poller correspondingly
  blocked 121.5 ms waiting for the lock. Stress A shows the other axis: at 1 Hz
  scraping the *mean* lock wait rose 30× (0.01 → 0.30 ms).
- **#3 per-column walk** — SNMP latency tracks interface count (1.5 → 4.8 ms as
  ifaces/device went 16 → 128): each column walk needs `ceil(ifaces/20)` more
  GETBULKs.

Reproduce: `python3 perf/run.py` (baseline);
`--interfaces 16 --poll-msecs 1500 --prometheus-interval 1` (A);
`--interfaces 128 --prometheus-interval 3` (B).

## Results after fixing #1 and #2

- **#1** — `report_interfaces` now clones the device interface map once per
  report instead of once per interface (O(interfaces) instead of
  O(interfaces²)); the LAG-peer statuses for link-flap events are read from that
  single immutable snapshot.
- **#2** — `/dev/metrics` now takes only a cheap owned snapshot
  (`IMDS::metrics_snapshot`) under the global lock and builds the
  `LabeledMetric` list (`IMDS::metrics_from`) *outside* it, so a scrape no longer
  blocks every poller for its full duration.

Same configs, same machine, before → after:

| Signal | Stress A (250×16 @1.5s, 1 Hz scrape) | Stress B (250×128 = 32k ifaces) |
|---|---|---|
| mean IMDS lock wait | 0.30 → **0.00 ms** | 0.01 → 0.01 ms |
| max IMDS lock wait  | 28.4 → **0.5 ms** | 121.5 → **3.5 ms** |
| metrics build (under lock) | 31.3 → **0.9 ms** | 141.9 → **6.8 ms** |
| report hold | 0.03 → 0.01 ms | 1.22 → **0.07 ms** |

Both hot-path serialization costs drop by 20–35×. Metric output is byte-identical
(the exact-string metric unit tests still pass).

## Results after fixing #3

- **#3** — `build_table` now walks all columns in **lockstep**: one multi-varbind
  GETBULK per round advances every still-active column at once
  (`Transport::getbulk_multi`), instead of a separate GETBULK sequence per
  column. Per-column robustness (prefix boundary, end-of-view, looping-agent
  guard, row cap) is preserved. Round-trips per device drop from
  `~columns × ceil(rows/max_rep)` to `~ceil(rows/max_rep)` — a ~column-count
  reduction (roughly 15–40× for ifTable/ifXTable).

Stress B (250×128 = 32k interfaces), before → after #3:

| Signal | before | after |
|---|---|---|
| SNMP latency mean | 4.8 ms | **3.6 ms** |
| SNMP latency max | 33.3 ms | **12.5 ms** |
| poll iteration mean | 10.5 ms | **8.4 ms** |

On loopback (sub-ms RTT) the latency win is modest; the real benefit is the
round-trip *count* collapse, which dominates on a live network where each RTT is
milliseconds. Verified end-to-end against the real snmp2 client with 0 SNMP
errors and all 32k interfaces reported.

Measured directly with the simulator's RTT injection (`--snmp-delay-ms 10
--snmp-jitter-ms 5`, same fleet, pre-#3 binary via `--nexus-bin`):

| Signal (250×128 @ 10 ms±5 ms RTT) | pre-#3 (per-column) | post-#3 (lockstep) |
|---|---|---|
| SNMP latency / table | **1063 ms** | **117 ms** |
| poll iteration / device | **2127 ms** | **236 ms** |

~9× under realistic latency. Consequence for the cycle budget: pre-#3 already
spent 2.1 s per device at only 10 ms RTT, so it would start overrunning the 10 s
cycle around ~45–55 ms RTT; post-#3 has roughly 9× that RTT headroom. (Both still
showed 0 overruns at 10 ms because thread-per-device gives each switch its own
10 s budget.)

## Results after fixing #5

- **#5** — `SnmpbotHttp` now builds one `reqwest::blocking::Client` lazily (on
  first use, off the async runtime) and reuses it, instead of constructing one
  per request. This keeps HTTP/1.1 keep-alive connections to snmpbot warm and
  drops the per-request tokio-runtime spin-up + TCP handshake.

Measured in **snmpbot mode** (nexus → local snmpbot → fleet), 250×16 @ 2 s cycle
(~250 HTTP requests/s to snmpbot), loopback, same fleet, pre-#5 binary via
`--nexus-bin`:

| Signal | pre-#5 (client/request) | post-#5 (shared) |
|---|---|---|
| SNMP latency / table (HTTP call) | 39.3 ms | **34.3 ms** |
| poll iteration / device | 78.8 ms | **68.9 ms** |
| poll iteration max | 183.8 ms | **144.1 ms** |

~5 ms saved per HTTP request, ~13% off the poll iteration. On loopback the HTTP
transport is near-free, so that ~5 ms is essentially the client-construction
cost (a private tokio runtime per call) the shared client removes; the tail
improves more because the fix also eliminates ~250 runtime/thread spawns per
second. No regression from sharing one client's runtime across the poller
threads (still 100% of target, better tail). Run it with
`perf/run.py --snmp-mode snmpbot`.

## Results after fixing #4

- **#4** — `SnmpSource` now owns an optional counting semaphore
  (`JASPY_SNMP_MAX_INFLIGHT`, 0 = unlimited) that every collector's SNMP call
  passes through, capping the *total* concurrent requests all collectors
  (interface poller + entity/vlan/lag) may have outstanding toward the shared
  back end. New perf gauges expose peak concurrency and permit-wait time
  (`jaspy_perf_snmp_inflight_max`, `jaspy_perf_snmp_permit_wait_*`).

Demonstration (embedded, 250×16 @ 1.5 s, 50±20 ms RTT, jitter **off** to force an
aligned 250-thread burst — the pathological alignment #4 warns about):

| | uncapped | capped 32 |
|---|---|---|
| peak concurrent SNMP | **250** | **32** |
| SNMP queries/s | 14 | **68** |
| interfaces/s | 109 | **545** |
| permit wait mean | 0 | 259 ms |

Uncapped, all 250 poll threads hit the back end at once; the burst overruns it
(here, loopback UDP buffers drop packets and the 3 s timeout+retry stalls
collapse throughput to ~4% of target — 0 "errors", just near-zero progress). The
cap bounds concurrency to 32, avoids the overload, and recovers ~5× the
throughput; permit-wait quantifies the smoothing.

Default is 0 (unlimited) so nothing changes out of the box — the poller's
per-thread start jitter already spreads normal load, so the cap is a **safety
valve** against pathological alignment (and against the extra collectors piling
on), which operators tune to their back end. `perf/run.py --snmp-max-inflight N`
drives it.

## Results after fixing #6

- **#6** — the embedded client opened a fresh UDP socket (`snmp2` session) on
  every `table()`/`object()` call. It now keeps a **thread-local session cache
  keyed by destination**: the thread-per-device interface poller opens one
  socket per device and reuses it across ifTable+ifXTable and across every
  cycle; a `run_bounded` worker reuses a session while it holds a device. snmp2
  sessions are single-owner (one request/reply socket), so per-thread is the
  natural unit and needs no locking; UDP sockets don't break on timeout, so a
  cached session stays valid. New counter `jaspy_perf_snmp_session_opens_total`.

Measured (embedded, 250×16 @ 2 s, 10±5 ms RTT):

| | before | after |
|---|---|---|
| socket opens | 1 per request (~249/s) | **~0/s steady state** (one per device at startup) |
| SNMP latency mean | 17.8 ms | 16.9 ms |

Socket opens go from **O(requests)** to **O(devices)** — ~249/s → 0/s once
sessions are warm (≈250 one-time opens for the whole fleet, confirmed by
capturing startup: 16.5 opens/s × 15 s ≈ 248 ≈ device count, against ~3,500
queries in the same window). Latency barely moves because socket setup is
microseconds; the win is fd/syscall churn and ephemeral-port pressure, which
matters on a busy host and at high request rates.

## Results after fixing #7

- **#7** — the poller supervisor called `Device::monitored()` (a full table
  query) every second just to detect device add/remove. It now reconciles the
  device set every `JASPY_POLLER_RELOAD_SECS` (default 15), while finished-thread
  reaping still runs every second. Not perf-measurable in a meaningful way (the
  query is sub-millisecond on sqlite) — it just removes one wasted DB query per
  second per process. Tests set it to 1 s to keep device pickup fast.

All findings #1–#7 are now addressed. Summary of what each delivered:

| # | fix | measured effect |
|---|---|---|
| 1 | hoist O(N²) clone out of report loop | report hold 1.22 → 0.07 ms @ 32k ifaces |
| 2 | build metrics outside the IMDS lock | max lock wait 121 → 3.5 ms; build 142 → 6.8 ms |
| 3 | multi-column lockstep GETBULK | SNMP latency 1063 → 117 ms @ 10 ms RTT (~9×) |
| 4 | global in-flight cap | bounds burst concurrency; ~5× throughput under overrun |
| 5 | reuse one reqwest client | SNMP latency 39 → 34 ms; ~250 runtime spawns/s removed |
| 6 | reuse embedded SNMP sessions | socket opens O(requests) → O(devices) |
| 7 | reconcile devices every 15 s not 1 s | −1 DB query/s |

None of #1–#7 was required to meet the 250/4,000/10 s baseline (which already
passed) — together they remove the ceilings that appear under scale, real RTT,
Prometheus scraping, and burst alignment, and add the instrumentation to see all
of it (`GET /dev/metrics/perf`, `perf/`).
