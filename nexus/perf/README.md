# nexus SNMP polling perf suite

Drives **real SNMP UDP traffic** from nexus (embedded client) against a
simulated switch fleet on loopback, then scrapes nexus's Prometheus/perf
metrics to measure whether the hot path sustains the target:

> **250 switches / 4,000 active interfaces / 10 s cycle.**

Purpose: capture a **baseline with today's code before optimizing**, then re-run
after each fix in `PERF.md` and diff the same counters.

## Pieces

| File | What |
|---|---|
| `src/main.rs` (`snmpsim`) | SNMP v2c fleet simulator. One UDP socket per switch on a distinct loopback IP, one shared non-privileged port. Serves a synthetic IF-MIB (ifTable + ifXTable) with monotonic counters. Requests parsed by the same `snmp2` nexus uses; responses BER-encoded by hand and round-trip-tested against `snmp2`'s parser. |
| `run.py` | Orchestrator + Prometheus scraper. Builds binaries, starts the fleet + nexus, seeds the DB, warms up, samples `/dev/metrics/perf` continuously, scrapes the heavy `/dev/metrics` on an interval (to reproduce scrape-vs-poll lock contention), prints a report. stdlib only. |
| `setup-loopback-macos.sh` | macOS only: alias the extra loopback IPs (Linux needs no setup). |

In-process instrumentation lives in nexus at `src/utilities/perfstats.rs`,
exposed at `GET /dev/metrics/perf` (lock-free atomics; negligible overhead so it
doesn't distort what it measures). New nexus config `JASPY_SNMP_PORT` lets the
embedded client target the simulator's unprivileged port.

## Run it

### Linux (native — 127.0.0.0/8 is all loopback)

```sh
cd nexus/perf
python3 run.py                 # builds everything, then 250 x 16 @ 10s for 60s
```

### macOS (alias the fleet's loopback IPs first)

```sh
cd nexus/perf
sudo ./setup-loopback-macos.sh 250      # alias 127.0.0.2 .. 127.0.1.251
python3 run.py
sudo ./setup-loopback-macos.sh down 250 # teardown when done
```

### Useful flags

```
--switches N          fleet size (default 250)
--interfaces M        interfaces per switch (default 16 -> 4000 total)
--poll-msecs MS       nexus poll cycle (default 10000)
--duration S          measurement window (default 60)
--warmup S            settle time before measuring (default 25)
--prometheus-interval S   heavy /dev/metrics scrape period (default 15; 0 = off)
--snmp-delay-ms MS    simulated per-round-trip network RTT at the fleet (default 0 = loopback)
--snmp-jitter-ms MS   uniform [0,J] jitter added to each simulated RTT
--snmp-timeout-ms MS  nexus per-request SNMP timeout (default 2000; must exceed the RTT)
--keep-collectors     leave entity/vlan/lag pollers on (default: isolate the interface poller)
--no-build            skip cargo build (use existing release binaries)
--nexus-bin PATH      run a specific jaspy-nexus binary (to A/B an older build)
--workdir DIR         keep db + logs here (default: a temp dir)
```

### Modelling real network latency

Loopback RTT is sub-millisecond, which hides the cost of SNMP round-trips. The
simulator can hold each response to model a real network:

```sh
# 250 x 128 interfaces, 10 ms RTT +/- 5 ms jitter
python3 run.py --interfaces 128 --snmp-delay-ms 10 --snmp-jitter-ms 5 --snmp-timeout-ms 3000
```

`--snmp-delay-ms` is added to **every round-trip**, so reported SNMP latency
becomes `~rounds_per_table x RTT`. This is what makes the per-column-walk cost
(and the #3 lockstep fix) visible: a walk that needs `columns x rounds`
round-trips scales with the delay, while the lockstep walk needs only `rounds`.
Keep the delay below `--snmp-timeout-ms` or every GETBULK times out.

To A/B a fix against an older build at the same RTT, build the old binary in a
git worktree and point `--nexus-bin` at it (the simulator is unchanged between
runs, so it's a fair comparison).

## Reading the report

```
  device polls/s        <achieved>  (target N/(poll_msecs/1000), % of target)
  poll OVERRUNS         iterations that exceeded the cycle budget   <- must be ~0
  SNMP latency          per table/object request (embedded per-column walk cost, PERF.md #3)
  IMDS lock WAIT        time pollers block on the global mutex       <- PERF.md #2 signal
  IMDS report hold      time held in report_interfaces (the O(N^2) clone, PERF.md #1)
  metrics build (max)   time building the metric Vec under the lock  <- PERF.md #2
```

**Interpreting it:**
- `poll OVERRUNS > 0` or `device polls/s` well under target ⇒ the fleet can't be
  polled within the cycle: the headline scalability failure.
- High **IMDS lock WAIT** ⇒ pollers serialize on the global mutex (#2). Grows
  with fleet size and with Prometheus scrape frequency.
- High **IMDS report hold** that scales super-linearly with interfaces-per-device
  ⇒ the O(N²) clone (#1).
- High **SNMP latency** in embedded mode vs snmpbot ⇒ per-column walk (#3).

## Notes / limitations

- The simulator serves **IF-MIB only**. `--keep-collectors` measures the
  concurrency overhead of the entity/vlan/lag pollers, but their tables return
  end-of-MIB immediately, so it does not reproduce their real SNMP cost.
- Contention (#2) only appears with a concurrent fleet; a single switch shows
  ~0 lock wait. Run the full 250 to see it.
- Counters are monotonic; the scraper diffs the first and last sample of the
  measurement window, so warmup is excluded.
- To baseline snmpbot mode instead of embedded, point `JASPY_SNMP_MODE=snmpbot`
  at a real snmpbot — out of scope for this loopback harness, which targets the
  embedded UDP path.
