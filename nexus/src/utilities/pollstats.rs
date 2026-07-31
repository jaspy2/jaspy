// Per-device, per-query SNMP poll counters, exposed as Prometheus series on
// /dev/metrics so the polling load can be attributed and rated. Unlike the
// fleet-aggregate jaspy_perf_snmp_* counters (perfstats.rs), these carry
// {fqdn, query} labels and are recorded at the SnmpSource seam (snmp/source.rs),
// so they cover EVERY collector (interface poller + entity/vlan/lag/discovery),
// not just the interface poller.
//
// Cardinality (200-device fleet): the VLAN is encoded in the host, not the
// `query` label, so the per-VLAN Cisco STP fan-out raises a series' counter
// value rather than adding series. Distinct query-ids/device ~20, five counters
// => ~20k series, a ~15-20% add on top of what /dev/metrics already emits.
//
// The registry follows the read-lock-fast-path / write-lock-to-insert pattern
// of snmp::embedded's DEVICE_LATENCY. The core logic is written against an
// injected `Registry` so it is unit-testable without touching global state.
use crate::snmp::types::{SNMPBotObjectResponse, SNMPBotResponse};
use crate::utilities::perfstats::SnmpOutcome;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

#[derive(Default)]
struct QueryCounters {
    requests: AtomicU64,  // every table/object call (ok, error, or timeout)
    values: AtomicU64,    // metric data points fetched (ok only)
    errors: AtomicU64,    // genuine (non-timeout) errors
    timeouts: AtomicU64,  // device-not-responding timeouts
    nanos: AtomicU64,     // time in non-timeout calls (ns)
}

impl QueryCounters {
    fn snapshot(&self) -> [u64; 5] {
        [
            self.requests.load(Ordering::Relaxed),
            self.values.load(Ordering::Relaxed),
            self.errors.load(Ordering::Relaxed),
            self.timeouts.load(Ordering::Relaxed),
            self.nanos.load(Ordering::Relaxed),
        ]
    }
}

type Registry = RwLock<HashMap<String, Arc<QueryCounters>>>;

static POLL_STATS: OnceLock<Registry> = OnceLock::new();

fn global() -> &'static Registry {
    POLL_STATS.get_or_init(|| RwLock::new(HashMap::new()))
}

// fqdn + query id joined by the ASCII Unit Separator (same keying idea as
// embedded::session_key), split back apart on render.
fn make_key(fqdn: &str, query: &str) -> String {
    format!("{}\u{1f}{}", fqdn, query)
}

// Number of metric data points in a table response: rows x columns actually
// present. One per (row, column) cell.
pub fn table_value_count(resp: &SNMPBotResponse) -> u64 {
    resp.entries.iter().map(|e| e.objects.len() as u64).sum()
}

// Number of metric data points in a scalar-object response.
pub fn object_value_count(resp: &SNMPBotObjectResponse) -> u64 {
    resp.instances.len() as u64
}

fn get_or_create(reg: &Registry, key: &str) -> Arc<QueryCounters> {
    if let Ok(map) = reg.read() {
        if let Some(c) = map.get(key) {
            return c.clone();
        }
    }
    let mut map = reg.write().unwrap();
    map.entry(key.to_string()).or_insert_with(|| Arc::new(QueryCounters::default())).clone()
}

fn record_in(reg: &Registry, fqdn: &str, query: &str, outcome: SnmpOutcome, values: u64, elapsed: Duration) {
    let counters = get_or_create(reg, &make_key(fqdn, query));
    counters.requests.fetch_add(1, Ordering::Relaxed);
    match outcome {
        SnmpOutcome::Ok => {
            counters.values.fetch_add(values, Ordering::Relaxed);
            counters.nanos.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
        }
        // A genuine error is a real (timed) response; keep it out of the value
        // count but in the latency, matching PERF's bucketing.
        SnmpOutcome::Error => {
            counters.errors.fetch_add(1, Ordering::Relaxed);
            counters.nanos.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
        }
        // A timeout's ~full-timeout wall time would skew the latency, so it is
        // only counted, not timed (again mirroring PERF).
        SnmpOutcome::Timeout => {
            counters.timeouts.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn retain_in(reg: &Registry, monitored: &HashSet<String>) {
    if let Ok(mut map) = reg.write() {
        map.retain(|key, _| {
            let fqdn = key.split('\u{1f}').next().unwrap_or("");
            monitored.contains(fqdn)
        });
    }
}

// Escape a Prometheus label value (\, ", newline). fqdns/query-ids are normally
// clean, but be safe.
fn escape(v: &str) -> String {
    v.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn render_from(reg: &Registry) -> String {
    // Snapshot to owned rows, then drop the read lock before formatting.
    let mut rows: Vec<(String, String, [u64; 5])> = match reg.read() {
        Ok(map) => map
            .iter()
            .map(|(key, c)| {
                let mut parts = key.splitn(2, '\u{1f}');
                let fqdn = parts.next().unwrap_or("").to_string();
                let query = parts.next().unwrap_or("").to_string();
                (fqdn, query, c.snapshot())
            })
            .collect(),
        Err(_) => return String::new(),
    };
    rows.sort();

    let families: [(&str, &str, usize); 5] = [
        ("jaspy_poll_snmp_requests_total", "SNMP table/object requests issued, by device and query", 0),
        ("jaspy_poll_snmp_values_total", "SNMP metric data points fetched, by device and query", 1),
        ("jaspy_poll_snmp_errors_total", "SNMP requests returning a genuine (non-timeout) error", 2),
        ("jaspy_poll_snmp_timeouts_total", "SNMP requests where the device did not respond", 3),
        ("jaspy_poll_snmp_nanos_total", "Time spent in non-timeout SNMP requests (ns)", 4),
    ];

    let mut s = String::with_capacity(rows.len() * 5 * 96 + 512);
    for (name, help, idx) in families {
        s.push_str(&format!("# HELP {} {}\n# TYPE {} counter\n", name, help, name));
        for (fqdn, query, vals) in &rows {
            s.push_str(&format!("{}{{fqdn=\"{}\",query=\"{}\"}} {}\n", name, escape(fqdn), escape(query), vals[idx]));
        }
    }
    s
}

// --- public API over the process-global registry ---------------------------

// Record one SNMP request against a device's per-query counters. Call from the
// SnmpSource seam so every collector is covered.
pub fn record(fqdn: &str, query: &str, outcome: SnmpOutcome, values: u64, elapsed: Duration) {
    record_in(global(), fqdn, query, outcome, values, elapsed);
}

// Drop counters for devices no longer monitored, so their series don't linger.
pub fn retain(monitored: &HashSet<String>) {
    retain_in(global(), monitored);
}

// Prometheus exposition text for all per-device/query series.
pub fn render() -> String {
    render_from(global())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snmp::types::{SNMPBotObjectInstance, SNMPBotResponse, SNMPBotResultEntry};
    use std::collections::HashMap;

    fn fresh() -> Registry {
        RwLock::new(HashMap::new())
    }

    fn entry(cols: usize) -> SNMPBotResultEntry {
        let mut objects = HashMap::new();
        for i in 0..cols {
            objects.insert(format!("col{}", i), crate::snmp::types::SNMPBotResultEntryObjectValue::Str(String::new()));
        }
        SNMPBotResultEntry { host_i_d: "h".to_string(), index: HashMap::new(), objects }
    }

    #[test]
    fn table_value_count_is_rows_times_columns() {
        let resp = SNMPBotResponse {
            i_d: "IF-MIB::ifTable".to_string(),
            index_keys: vec![],
            object_keys: vec![],
            entries: vec![entry(3), entry(3), entry(2)], // 3 + 3 + 2 = 8 cells
        };
        assert_eq!(table_value_count(&resp), 8);
    }

    #[test]
    fn object_value_count_is_instance_count() {
        let resp = SNMPBotObjectResponse {
            i_d: "SNMPv2-MIB::sysName".to_string(),
            instances: vec![SNMPBotObjectInstance { value: None }],
        };
        assert_eq!(object_value_count(&resp), 1);
    }

    #[test]
    fn record_accumulates_per_outcome_and_separates_series() {
        let reg = fresh();
        record_in(&reg, "sw1.example.com", "ifTable", SnmpOutcome::Ok, 10, Duration::from_nanos(500));
        record_in(&reg, "sw1.example.com", "ifTable", SnmpOutcome::Ok, 5, Duration::from_nanos(500));
        record_in(&reg, "sw1.example.com", "ifTable", SnmpOutcome::Error, 0, Duration::from_nanos(200));
        record_in(&reg, "sw1.example.com", "ifTable", SnmpOutcome::Timeout, 0, Duration::from_nanos(0));
        // A different query is a separate series.
        record_in(&reg, "sw1.example.com", "entPhySensorTable", SnmpOutcome::Ok, 7, Duration::from_nanos(100));

        let c = get_or_create(&reg, &make_key("sw1.example.com", "ifTable"));
        let [requests, values, errors, timeouts, nanos] = c.snapshot();
        assert_eq!(requests, 4); // ok + ok + error + timeout
        assert_eq!(values, 15); // 10 + 5 (errors/timeouts add no values)
        assert_eq!(errors, 1);
        assert_eq!(timeouts, 1);
        assert_eq!(nanos, 1200); // 500 + 500 (ok) + 200 (error); timeout not timed

        let other = get_or_create(&reg, &make_key("sw1.example.com", "entPhySensorTable"));
        assert_eq!(other.snapshot()[1], 7);
    }

    #[test]
    fn render_emits_labeled_counter_lines() {
        let reg = fresh();
        record_in(&reg, "sw1.example.com", "ifTable", SnmpOutcome::Ok, 10, Duration::from_nanos(500));
        let text = render_from(&reg);
        assert!(text.contains("# TYPE jaspy_poll_snmp_values_total counter"));
        assert!(text.contains("jaspy_poll_snmp_values_total{fqdn=\"sw1.example.com\",query=\"ifTable\"} 10"));
        assert!(text.contains("jaspy_poll_snmp_requests_total{fqdn=\"sw1.example.com\",query=\"ifTable\"} 1"));
    }

    #[test]
    fn retain_drops_unmonitored_devices() {
        let reg = fresh();
        record_in(&reg, "keep.example.com", "ifTable", SnmpOutcome::Ok, 1, Duration::from_nanos(1));
        record_in(&reg, "drop.example.com", "ifTable", SnmpOutcome::Ok, 1, Duration::from_nanos(1));
        let monitored: HashSet<String> = ["keep.example.com".to_string()].iter().cloned().collect();
        retain_in(&reg, &monitored);
        let text = render_from(&reg);
        assert!(text.contains("fqdn=\"keep.example.com\""));
        assert!(!text.contains("fqdn=\"drop.example.com\""));
    }
}
