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

### 4. 🟠 Three new default-on collectors share the one snmpbot
`main.rs` — `entitypoller`/`vlanpoller`/`lagpoller` (each `unwrap_or(true)`) add
up to 16 workers each against the sidecar the 250-thread poller already loads.
No global in-flight budget; jitter is `interval/2`, so the 120 s/300 s cycles
re-cluster over time. Most likely cause of a regression from the baseline.
- **Fix:** shared semaphore capping total in-flight requests to snmpbot; stagger
  collector phases.

### 5. 🟠 A fresh reqwest blocking client per SNMP request (no keep-alive)
`snmp/snmpbot_http.rs::client()` builds a new `reqwest::blocking::Client` (own
tokio runtime, fresh TCP, no pool) on **every** table/object call. Thousands of
runtime-spawn + TCP-setup/teardown cycles per 10 s. Preserved from the old
`reqwest::blocking::get`, but a real ceiling.
- **Fix:** one reused `Client` (or one per worker thread) with keep-alive.

### 6. 🟡 Embedded opens a new UDP socket/session per table and per object
`snmp/embedded.rs::table`/`object` call `SnmpSession::open` each time. Cache a
session per (host, cycle).

### 7. 🟡 Poller supervisor reloads all monitored devices from the DB every 1 s
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

Remaining findings (#4 shared snmpbot budget, #5 per-request HTTP client, #6/#7)
are untouched.
