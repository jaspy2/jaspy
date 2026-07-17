// Lock-free, process-global performance counters for the polling hot path.
//
// Everything here is plain atomics updated with Relaxed ordering, so the
// instrumentation itself adds no lock contention and negligible cost — it must
// not distort the very bottlenecks it measures (chiefly the single IMDS mutex,
// finding #2 in PERF.md). Rendered as Prometheus text at GET /dev/metrics/perf
// and scraped by perf/run.py.
//
// Timings are accumulated as total nanoseconds + a sample count, so the scraper
// derives a mean by differencing two scrapes (delta_nanos / delta_count). A
// per-metric max is kept via fetch_max for tail visibility.
use std::sync::atomic::{AtomicU64, Ordering};

pub struct PerfStats {
    // Interface poller (collectors/poller.rs).
    pub device_polls: AtomicU64,          // completed per-device poll iterations
    pub poll_overruns: AtomicU64,         // iterations whose wall time exceeded the cycle budget
    pub poll_iter_nanos: AtomicU64,       // total per-iteration wall time
    pub poll_iter_max_nanos: AtomicU64,   // slowest single iteration

    // SNMP back end (both snmpbot and embedded, via snmp_query).
    pub snmp_queries: AtomicU64,          // table/object requests attempted
    pub snmp_query_errors: AtomicU64,     // requests that returned Err (timeout etc.)
    pub snmp_query_nanos: AtomicU64,      // total time spent in SnmpSource calls
    pub snmp_query_max_nanos: AtomicU64,  // slowest single request

    // Embedded SNMP session opens (UDP socket creations). With per-call opening
    // this tracks the request count; with session reuse it drops to ~one per
    // (thread, device) (PERF.md #6).
    pub snmp_session_opens: AtomicU64,

    // SNMP concurrency toward the shared back end (PERF.md #4).
    pub snmp_inflight: AtomicU64,          // requests currently in the back end (gauge)
    pub snmp_inflight_max: AtomicU64,      // peak concurrent requests (high-water)
    pub snmp_permit_waits: AtomicU64,      // acquisitions of the in-flight limiter
    pub snmp_permit_wait_nanos: AtomicU64, // total time blocked on the limiter
    pub snmp_permit_wait_max_nanos: AtomicU64,

    // IMDS global mutex (the central serialization point).
    pub imds_lock_wait_nanos: AtomicU64,  // time blocked acquiring the lock in the poller
    pub imds_lock_wait_max_nanos: AtomicU64,
    pub imds_report_nanos: AtomicU64,     // time holding the lock in report_interfaces
    pub interfaces_reported: AtomicU64,   // interface rows written into IMDS

    // Prometheus scrape (routes/dev/metrics.rs).
    pub metrics_scrapes: AtomicU64,
    pub metrics_build_nanos: AtomicU64,   // time building the metric Vec under the lock
    pub metrics_build_max_nanos: AtomicU64,
}

impl PerfStats {
    const fn new() -> PerfStats {
        PerfStats {
            device_polls: AtomicU64::new(0),
            poll_overruns: AtomicU64::new(0),
            poll_iter_nanos: AtomicU64::new(0),
            poll_iter_max_nanos: AtomicU64::new(0),
            snmp_queries: AtomicU64::new(0),
            snmp_query_errors: AtomicU64::new(0),
            snmp_query_nanos: AtomicU64::new(0),
            snmp_query_max_nanos: AtomicU64::new(0),
            snmp_session_opens: AtomicU64::new(0),
            snmp_inflight: AtomicU64::new(0),
            snmp_inflight_max: AtomicU64::new(0),
            snmp_permit_waits: AtomicU64::new(0),
            snmp_permit_wait_nanos: AtomicU64::new(0),
            snmp_permit_wait_max_nanos: AtomicU64::new(0),
            imds_lock_wait_nanos: AtomicU64::new(0),
            imds_lock_wait_max_nanos: AtomicU64::new(0),
            imds_report_nanos: AtomicU64::new(0),
            interfaces_reported: AtomicU64::new(0),
            metrics_scrapes: AtomicU64::new(0),
            metrics_build_nanos: AtomicU64::new(0),
            metrics_build_max_nanos: AtomicU64::new(0),
        }
    }

    #[inline]
    fn add(counter: &AtomicU64, v: u64) {
        counter.fetch_add(v, Ordering::Relaxed);
    }

    #[inline]
    fn max(counter: &AtomicU64, v: u64) {
        counter.fetch_max(v, Ordering::Relaxed);
    }

    // Record one SNMP request: its wall time and whether it failed.
    pub fn record_snmp(&self, elapsed: std::time::Duration, ok: bool) {
        let ns = elapsed.as_nanos() as u64;
        Self::add(&self.snmp_queries, 1);
        if !ok {
            Self::add(&self.snmp_query_errors, 1);
        }
        Self::add(&self.snmp_query_nanos, ns);
        Self::max(&self.snmp_query_max_nanos, ns);
    }

    // A request entered the back end (permit already held). Updates the current
    // gauge and the peak high-water mark.
    pub fn inflight_enter(&self) {
        let now = self.snmp_inflight.fetch_add(1, Ordering::Relaxed) + 1;
        Self::max(&self.snmp_inflight_max, now);
    }

    pub fn inflight_exit(&self) {
        self.snmp_inflight.fetch_sub(1, Ordering::Relaxed);
    }

    // Time spent blocked acquiring an in-flight permit (near zero when the cap
    // isn't the bottleneck; grows when the fleet would otherwise overrun the
    // back end).
    pub fn record_permit_wait(&self, elapsed: std::time::Duration) {
        let ns = elapsed.as_nanos() as u64;
        Self::add(&self.snmp_permit_waits, 1);
        Self::add(&self.snmp_permit_wait_nanos, ns);
        Self::max(&self.snmp_permit_wait_max_nanos, ns);
    }

    pub fn record_session_open(&self) {
        Self::add(&self.snmp_session_opens, 1);
    }

    pub fn record_lock_wait(&self, elapsed: std::time::Duration) {
        let ns = elapsed.as_nanos() as u64;
        Self::add(&self.imds_lock_wait_nanos, ns);
        Self::max(&self.imds_lock_wait_max_nanos, ns);
    }

    pub fn record_report(&self, elapsed: std::time::Duration, interfaces: u64) {
        Self::add(&self.imds_report_nanos, elapsed.as_nanos() as u64);
        Self::add(&self.interfaces_reported, interfaces);
    }

    // Record one completed poll iteration: its wall time and whether it
    // overran the cycle budget.
    pub fn record_poll_iter(&self, elapsed: std::time::Duration, budget_msecs: u64) {
        let ns = elapsed.as_nanos() as u64;
        Self::add(&self.device_polls, 1);
        Self::add(&self.poll_iter_nanos, ns);
        Self::max(&self.poll_iter_max_nanos, ns);
        if elapsed.as_millis() as u64 > budget_msecs {
            Self::add(&self.poll_overruns, 1);
        }
    }

    pub fn record_metrics_build(&self, elapsed: std::time::Duration) {
        let ns = elapsed.as_nanos() as u64;
        Self::add(&self.metrics_scrapes, 1);
        Self::add(&self.metrics_build_nanos, ns);
        Self::max(&self.metrics_build_max_nanos, ns);
    }

    // Prometheus exposition text. Counters are monotonic; the scraper diffs two
    // snapshots to get rates and per-op means.
    pub fn render(&self) -> String {
        let g = |c: &AtomicU64| c.load(Ordering::Relaxed);
        let mut s = String::with_capacity(2048);
        let line = |name: &str, help: &str, kind: &str, val: u64, out: &mut String| {
            out.push_str(&format!("# HELP {} {}\n# TYPE {} {}\n{} {}\n", name, help, name, kind, name, val));
        };
        line("jaspy_perf_device_polls_total", "Completed per-device poll iterations", "counter", g(&self.device_polls), &mut s);
        line("jaspy_perf_poll_overruns_total", "Poll iterations exceeding the cycle budget", "counter", g(&self.poll_overruns), &mut s);
        line("jaspy_perf_poll_iter_nanos_total", "Total per-iteration poll wall time (ns)", "counter", g(&self.poll_iter_nanos), &mut s);
        line("jaspy_perf_poll_iter_max_nanos", "Slowest single poll iteration (ns)", "gauge", g(&self.poll_iter_max_nanos), &mut s);
        line("jaspy_perf_snmp_queries_total", "SNMP table/object requests attempted", "counter", g(&self.snmp_queries), &mut s);
        line("jaspy_perf_snmp_query_errors_total", "SNMP requests returning an error", "counter", g(&self.snmp_query_errors), &mut s);
        line("jaspy_perf_snmp_query_nanos_total", "Total time in SNMP back-end calls (ns)", "counter", g(&self.snmp_query_nanos), &mut s);
        line("jaspy_perf_snmp_query_max_nanos", "Slowest single SNMP request (ns)", "gauge", g(&self.snmp_query_max_nanos), &mut s);
        line("jaspy_perf_snmp_session_opens_total", "Embedded SNMP session (UDP socket) opens", "counter", g(&self.snmp_session_opens), &mut s);
        line("jaspy_perf_snmp_inflight", "SNMP requests currently in the back end", "gauge", g(&self.snmp_inflight), &mut s);
        line("jaspy_perf_snmp_inflight_max", "Peak concurrent SNMP requests in the back end", "gauge", g(&self.snmp_inflight_max), &mut s);
        line("jaspy_perf_snmp_permit_waits_total", "In-flight limiter acquisitions", "counter", g(&self.snmp_permit_waits), &mut s);
        line("jaspy_perf_snmp_permit_wait_nanos_total", "Total time blocked on the in-flight limiter (ns)", "counter", g(&self.snmp_permit_wait_nanos), &mut s);
        line("jaspy_perf_snmp_permit_wait_max_nanos", "Longest single in-flight-limiter wait (ns)", "gauge", g(&self.snmp_permit_wait_max_nanos), &mut s);
        line("jaspy_perf_imds_lock_wait_nanos_total", "Total time blocked acquiring the IMDS lock in the poller (ns)", "counter", g(&self.imds_lock_wait_nanos), &mut s);
        line("jaspy_perf_imds_lock_wait_max_nanos", "Longest single IMDS lock wait (ns)", "gauge", g(&self.imds_lock_wait_max_nanos), &mut s);
        line("jaspy_perf_imds_report_nanos_total", "Total time holding the IMDS lock in report_interfaces (ns)", "counter", g(&self.imds_report_nanos), &mut s);
        line("jaspy_perf_interfaces_reported_total", "Interface rows written into IMDS", "counter", g(&self.interfaces_reported), &mut s);
        line("jaspy_perf_metrics_scrapes_total", "Prometheus scrapes of /dev/metrics", "counter", g(&self.metrics_scrapes), &mut s);
        line("jaspy_perf_metrics_build_nanos_total", "Total time building the metric Vec under the IMDS lock (ns)", "counter", g(&self.metrics_build_nanos), &mut s);
        line("jaspy_perf_metrics_build_max_nanos", "Slowest single metrics build (ns)", "gauge", g(&self.metrics_build_max_nanos), &mut s);
        s
    }
}

// Process-global instance. `Instant` is a monotonic clock and is fine to use
// freely here.
pub static PERF: PerfStats = PerfStats::new();

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn render_contains_all_series() {
        let stats = PerfStats::new();
        stats.record_snmp(Duration::from_millis(3), true);
        stats.record_snmp(Duration::from_millis(9), false);
        stats.record_poll_iter(Duration::from_millis(50), 10_000);
        stats.record_poll_iter(Duration::from_millis(12_000), 10_000);
        stats.record_lock_wait(Duration::from_micros(400));
        stats.record_report(Duration::from_millis(2), 48);
        stats.record_metrics_build(Duration::from_millis(5));

        let text = stats.render();
        assert!(text.contains("jaspy_perf_snmp_queries_total 2"));
        assert!(text.contains("jaspy_perf_snmp_query_errors_total 1"));
        assert!(text.contains("jaspy_perf_device_polls_total 2"));
        // One of the two iterations overran the 10s budget.
        assert!(text.contains("jaspy_perf_poll_overruns_total 1"));
        assert!(text.contains("jaspy_perf_interfaces_reported_total 48"));
        // Max tracks the slower of the two SNMP calls (9ms).
        assert!(text.contains(&format!("jaspy_perf_snmp_query_max_nanos {}", 9_000_000u64)));
    }

    #[test]
    fn max_is_monotonic_high_water_mark() {
        let stats = PerfStats::new();
        stats.record_snmp(Duration::from_millis(5), true);
        stats.record_snmp(Duration::from_millis(2), true);
        assert_eq!(stats.snmp_query_max_nanos.load(Ordering::Relaxed), 5_000_000);
    }
}
