#!/usr/bin/env python3
"""
nexus SNMP polling perf harness.

Drives *real* SNMP UDP traffic from nexus (embedded client) against a simulated
switch fleet on loopback, then scrapes nexus's Prometheus/perf metrics to
measure whether the hot path sustains the target: 250 switches / 4000 interfaces
/ 10 s cycle. Use it to capture a baseline BEFORE optimizing, then re-run after
each fix and diff the same numbers.

What it does, in order:
  1. (optional) builds the snmpsim + jaspy-nexus release binaries
  2. starts the SNMP fleet simulator (perf/snmpsim)
  3. starts nexus in embedded SNMP mode against a fresh sqlite db
  4. seeds the db with N devices (fqdn 127.0.0.X) x M interfaces each
  5. warms up a few poll cycles
  6. runs a Prometheus scraper: samples /dev/metrics/perf continuously and
     scrapes the heavy /dev/metrics (4000-interface payload) on an interval to
     reproduce the scrape-vs-poll lock contention (PERF.md #2)
  7. prints a report and tears everything down

Linux runs 127.0.0.0/8 as loopback with no setup. On macOS run
perf/setup-loopback-macos.sh first (and pass --switches <= aliased count).

stdlib only; no pip installs.
"""
import argparse
import os
import signal
import sqlite3
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

PERF_DIR = Path(__file__).resolve().parent
NEXUS_DIR = PERF_DIR.parent
REPO_DIR = NEXUS_DIR.parent
DEFAULT_MIB_DIR = REPO_DIR / "snmpbot" / "mibs"


def log(msg):
    print(f"[perf] {msg}", flush=True)


def http_get(url, timeout=30):
    with urllib.request.urlopen(url, timeout=timeout) as r:
        return r.read().decode("utf-8", "replace")


def wait_http(url, timeout_s):
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        try:
            http_get(url, timeout=2)
            return True
        except Exception:
            time.sleep(0.25)
    return False


def parse_prom(text):
    """Parse Prometheus exposition text into {name: float}. Ignores labels
    (our perf series are unlabeled)."""
    out = {}
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split()
        if len(parts) >= 2:
            try:
                out[parts[0]] = float(parts[1])
            except ValueError:
                pass
    return out


def octet_to_ip(base_octet, n):
    """Match snmpsim's IP layout: 127.0.(x/256).(x%256), x = base_octet + n."""
    x = base_octet + n
    return f"127.0.{x // 256}.{x % 256}"


def seed_db(db_path, switches, interfaces, community, base_octet):
    """Insert N devices + N*M interfaces directly into nexus's sqlite db.
    nexus has already run migrations by the time it serves HTTP."""
    conn = sqlite3.connect(db_path, timeout=30)
    try:
        conn.execute("PRAGMA busy_timeout=30000")
        cur = conn.cursor()
        for n in range(switches):
            ip = octet_to_ip(base_octet, n)
            # fqdn = name.dns_domain = "127" + "." + "0.0.X" = the loopback ip.
            dns_domain = ip.split(".", 1)[1]  # "0.0.X"
            cur.execute(
                "INSERT INTO devices (name, dns_domain, snmp_community, polling_enabled) VALUES (?,?,?,1)",
                ("127", dns_domain, community),
            )
            device_id = cur.lastrowid
            cur.executemany(
                'INSERT INTO interfaces ("index", interface_type, device_id, name, polling_enabled) '
                "VALUES (?,?,?,?,1)",
                [(i, "ethernet", device_id, f"Gi0/{i}") for i in range(1, interfaces + 1)],
            )
        conn.commit()
    finally:
        conn.close()


def build_binaries():
    log("building snmpsim (release)...")
    subprocess.run(["cargo", "build", "--release"], cwd=PERF_DIR, check=True)
    log("building jaspy-nexus (release)... (first build is slow)")
    subprocess.run(
        ["cargo", "build", "--release", "--bin", "jaspy-nexus"], cwd=NEXUS_DIR, check=True
    )


class Proc:
    def __init__(self, name, argv, env, logfile):
        self.name = name
        self.logf = open(logfile, "w")
        self.p = subprocess.Popen(argv, env=env, stdout=self.logf, stderr=subprocess.STDOUT)

    def alive(self):
        return self.p.poll() is None

    def stop(self):
        if self.p.poll() is None:
            self.p.send_signal(signal.SIGINT)
            try:
                self.p.wait(timeout=8)
            except subprocess.TimeoutExpired:
                self.p.kill()
        self.logf.close()


def main():
    ap = argparse.ArgumentParser(description="nexus SNMP polling perf harness")
    ap.add_argument("--switches", type=int, default=250)
    ap.add_argument("--interfaces", type=int, default=16, help="interfaces per switch")
    ap.add_argument("--poll-msecs", type=int, default=10000, help="nexus poll cycle (ms)")
    ap.add_argument("--duration", type=int, default=60, help="measurement window (s)")
    ap.add_argument("--warmup", type=int, default=25, help="warmup before measuring (s)")
    ap.add_argument("--sample-interval", type=float, default=2.0, help="perf-counter sample period (s)")
    ap.add_argument("--prometheus-interval", type=float, default=15.0,
                    help="heavy /dev/metrics scrape period (s); 0 to disable")
    ap.add_argument("--snmp-port", type=int, default=16100)
    ap.add_argument("--base-octet", type=int, default=2)
    ap.add_argument("--snmp-delay-ms", type=int, default=0,
                    help="simulated per-round-trip network RTT at the fleet (default 0 = loopback)")
    ap.add_argument("--snmp-jitter-ms", type=int, default=0,
                    help="uniform [0,J] jitter added to each simulated RTT")
    ap.add_argument("--rocket-port", type=int, default=8710)
    ap.add_argument("--community", default="public")
    ap.add_argument("--mib-dir", default=str(DEFAULT_MIB_DIR))
    ap.add_argument("--snmp-timeout-ms", type=int, default=2000)
    ap.add_argument("--snmp-retries", type=int, default=1)
    ap.add_argument("--no-build", action="store_true")
    ap.add_argument("--nexus-bin", default="",
                    help="path to a jaspy-nexus binary to run (default: this tree's release build); "
                         "use to A/B an older build against the same fleet")
    ap.add_argument("--keep-collectors", action="store_true",
                    help="leave entity/vlan/lag pollers enabled (default: isolate the interface poller)")
    ap.add_argument("--workdir", default="", help="dir for db + logs (default: a temp dir)")
    args = ap.parse_args()

    if not args.no_build:
        build_binaries()

    snmpsim_bin = PERF_DIR / "target" / "release" / "snmpsim"
    nexus_bin = Path(args.nexus_bin) if args.nexus_bin else NEXUS_DIR / "target" / "release" / "jaspy-nexus"
    for b in (snmpsim_bin, nexus_bin):
        if not b.exists():
            log(f"missing binary {b}; run without --no-build")
            return 2

    workdir = Path(args.workdir) if args.workdir else Path(
        subprocess.check_output(["mktemp", "-d"]).decode().strip()
    )
    workdir.mkdir(parents=True, exist_ok=True)
    db_path = workdir / "perf.sqlite"
    if db_path.exists():
        db_path.unlink()
    log(f"workdir {workdir}")

    total_ifaces = args.switches * args.interfaces
    rtt = f", RTT {args.snmp_delay_ms}ms+[0,{args.snmp_jitter_ms}]" if args.snmp_delay_ms or args.snmp_jitter_ms else ""
    log(f"target: {args.switches} switches x {args.interfaces} ifaces = {total_ifaces} interfaces @ {args.poll_msecs}ms{rtt}")
    if args.snmp_delay_ms + args.snmp_jitter_ms >= args.snmp_timeout_ms:
        log(f"WARNING: simulated RTT ({args.snmp_delay_ms}+{args.snmp_jitter_ms}ms) >= snmp-timeout-ms "
            f"({args.snmp_timeout_ms}); each GETBULK will time out. Raise --snmp-timeout-ms.")

    # --- SNMP fleet simulator ---
    sim_env = dict(os.environ)
    sim_env.update(
        SNMPSIM_SWITCHES=str(args.switches),
        SNMPSIM_INTERFACES=str(args.interfaces),
        SNMPSIM_PORT=str(args.snmp_port),
        SNMPSIM_BASE_OCTET=str(args.base_octet),
        SNMPSIM_DELAY_MS=str(args.snmp_delay_ms),
        SNMPSIM_JITTER_MS=str(args.snmp_jitter_ms),
    )
    sim = Proc("snmpsim", [str(snmpsim_bin)], sim_env, workdir / "snmpsim.log")
    time.sleep(1.0)
    if not sim.alive():
        log(f"snmpsim died on startup; see {workdir}/snmpsim.log")
        return 2

    # --- nexus (embedded SNMP mode) ---
    nexus_env = dict(os.environ)
    collectors_off = "false" if not args.keep_collectors else "true"
    nexus_env.update(
        ROCKET_ADDRESS="127.0.0.1",
        ROCKET_PORT=str(args.rocket_port),
        JASPY_DB_URL=f"sqlite:{db_path}",
        JASPY_SNMP_MODE="embedded",
        JASPY_SNMP_PORT=str(args.snmp_port),
        JASPY_SNMP_MIB_DIR=args.mib_dir,
        JASPY_SNMP_TIMEOUT_MS=str(args.snmp_timeout_ms),
        JASPY_SNMP_RETRIES=str(args.snmp_retries),
        JASPY_POLL_LOOP_MSECS=str(args.poll_msecs),
        JASPY_ENABLE_POLLER="true",
        JASPY_ENABLE_PINGER="false",       # oping needs root; poller derives up/down
        JASPY_ENABLE_ENTITYPOLLER=collectors_off,
        JASPY_ENABLE_VLANPOLLER=collectors_off,
        JASPY_ENABLE_LAGPOLLER=collectors_off,
        JASPY_ENABLE_TRAP_RECEIVER="false",
        JASPY_IMDS_REFRESH_SECS="5",
    )
    base_url = f"http://127.0.0.1:{args.rocket_port}"
    nexus = Proc("nexus", [str(nexus_bin)], nexus_env, workdir / "nexus.log")

    report = {}
    try:
        log("waiting for nexus http...")
        if not wait_http(f"{base_url}/dev/metrics/perf", 60):
            log(f"nexus did not come up; see {workdir}/nexus.log")
            return 2

        log("seeding db...")
        seed_db(str(db_path), args.switches, args.interfaces, args.community, args.base_octet)

        log(f"warmup {args.warmup}s (device threads spin up + first cycles)...")
        time.sleep(args.warmup)

        # --- measurement window: sample perf counters + emulate Prometheus ---
        log(f"measuring for {args.duration}s...")
        first = parse_prom(http_get(f"{base_url}/dev/metrics/perf"))
        t_first = time.time()
        last, t_last = first, t_first
        last_prom = 0.0
        prom_scrapes = 0
        prom_bytes = 0
        deadline = time.time() + args.duration
        while time.time() < deadline:
            time.sleep(args.sample_interval)
            try:
                last = parse_prom(http_get(f"{base_url}/dev/metrics/perf"))
                t_last = time.time()
            except Exception as e:
                log(f"perf scrape failed: {e}")
            if args.prometheus_interval > 0 and time.time() - last_prom >= args.prometheus_interval:
                last_prom = time.time()
                try:
                    body = http_get(f"{base_url}/dev/metrics/", timeout=60)
                    prom_scrapes += 1
                    prom_bytes = len(body)
                except Exception as e:
                    log(f"prometheus scrape failed: {e}")

        report = build_report(first, last, t_last - t_first, args, total_ifaces,
                              prom_scrapes, prom_bytes)
    finally:
        nexus.stop()
        sim.stop()

    if report:
        print_report(report, args, workdir)
    return 0


def build_report(first, last, elapsed, args, total_ifaces, prom_scrapes, prom_bytes):
    def d(key):
        return last.get(key, 0.0) - first.get(key, 0.0)

    def gauge(key):
        return last.get(key, 0.0)

    polls = d("jaspy_perf_device_polls_total")
    overruns = d("jaspy_perf_poll_overruns_total")
    ifaces = d("jaspy_perf_interfaces_reported_total")
    snmp_q = d("jaspy_perf_snmp_queries_total")
    snmp_err = d("jaspy_perf_snmp_query_errors_total")

    def mean_ms(nanos_key, count):
        return (d(nanos_key) / count / 1e6) if count > 0 else 0.0

    target_polls_per_s = args.switches / (args.poll_msecs / 1000.0)
    return {
        "elapsed_s": elapsed,
        "target_polls_per_s": target_polls_per_s,
        "polls": polls,
        "polls_per_s": polls / elapsed if elapsed > 0 else 0.0,
        "overruns": overruns,
        "ifaces_reported": ifaces,
        "ifaces_per_s": ifaces / elapsed if elapsed > 0 else 0.0,
        "snmp_queries": snmp_q,
        "snmp_qps": snmp_q / elapsed if elapsed > 0 else 0.0,
        "snmp_err": snmp_err,
        "snmp_err_pct": (100.0 * snmp_err / snmp_q) if snmp_q > 0 else 0.0,
        "snmp_mean_ms": mean_ms("jaspy_perf_snmp_query_nanos_total", snmp_q),
        "snmp_max_ms": gauge("jaspy_perf_snmp_query_max_nanos") / 1e6,
        "poll_iter_mean_ms": mean_ms("jaspy_perf_poll_iter_nanos_total", polls),
        "poll_iter_max_ms": gauge("jaspy_perf_poll_iter_max_nanos") / 1e6,
        "lock_wait_mean_ms": mean_ms("jaspy_perf_imds_lock_wait_nanos_total", polls),
        "lock_wait_max_ms": gauge("jaspy_perf_imds_lock_wait_max_nanos") / 1e6,
        "report_mean_ms": mean_ms("jaspy_perf_imds_report_nanos_total", polls),
        "metrics_build_max_ms": gauge("jaspy_perf_metrics_build_max_nanos") / 1e6,
        "prom_scrapes": prom_scrapes,
        "prom_kb": prom_bytes / 1024.0,
    }


def print_report(r, args, workdir):
    achieved_pct = 100.0 * r["polls_per_s"] / r["target_polls_per_s"] if r["target_polls_per_s"] else 0.0
    verdict = "OK" if r["overruns"] == 0 and achieved_pct >= 98 else "UNDER TARGET"
    lines = [
        "",
        "=" * 68,
        f"  nexus SNMP polling perf report   [{verdict}]",
        "=" * 68,
        f"  fleet                 {args.switches} switches x {args.interfaces} ifaces "
        f"({args.switches * args.interfaces} interfaces)",
        f"  poll cycle            {args.poll_msecs} ms",
        f"  measured window       {r['elapsed_s']:.1f} s",
        "-" * 68,
        f"  device polls/s        {r['polls_per_s']:.1f}  (target {r['target_polls_per_s']:.1f}, "
        f"{achieved_pct:.0f}% of target)",
        f"  poll OVERRUNS         {int(r['overruns'])}   (iterations exceeding the {args.poll_msecs}ms budget)",
        f"  interfaces/s          {r['ifaces_per_s']:.0f}   (total reported {int(r['ifaces_reported'])})",
        "-" * 68,
        f"  SNMP queries/s        {r['snmp_qps']:.0f}",
        f"  SNMP errors           {int(r['snmp_err'])}  ({r['snmp_err_pct']:.1f}%)",
        f"  SNMP latency          mean {r['snmp_mean_ms']:.1f} ms   max {r['snmp_max_ms']:.1f} ms",
        f"  poll iteration        mean {r['poll_iter_mean_ms']:.1f} ms   max {r['poll_iter_max_ms']:.1f} ms",
        "-" * 68,
        f"  IMDS lock WAIT        mean {r['lock_wait_mean_ms']:.2f} ms   max {r['lock_wait_max_ms']:.1f} ms  <-- contention (PERF.md #2)",
        f"  IMDS report hold      mean {r['report_mean_ms']:.2f} ms",
        f"  metrics build (max)   {r['metrics_build_max_ms']:.1f} ms  (under the global lock)",
        f"  Prometheus scrapes    {r['prom_scrapes']} of /dev/metrics (~{r['prom_kb']:.0f} KB each)",
        "=" * 68,
        f"  logs + db: {workdir}",
        "",
    ]
    print("\n".join(lines))


if __name__ == "__main__":
    sys.exit(main())
