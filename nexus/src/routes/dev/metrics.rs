use std::sync::{Arc, Mutex};
use crate::utilities;
use crate::collectors::entitypoller::EntityMetricsStore;
use rocket::State;
use crate::models;
use rocket::get;
use rocket::serde::json::Json;

// TODO: GH#9 Move everything to v1 API
#[get("/fast")]
pub fn metrics_fast(
    imds: &State<Arc<Mutex<utilities::imds::IMDS>>>,
    polling: &State<Arc<crate::collectors::PollingControl>>,
) -> Option<String> {
    // Polling paused (Maintenance page master switch): emit nothing so
    // Prometheus stops scraping the frozen switch series instead of recording
    // stale values (see collectors::PollingControl).
    if !polling.enabled() {
        return Some("\n".to_string());
    }
    let mut ret : String = String::new();
    let metrics : Option<Vec<models::metrics::LabeledMetric>>;

    if let Ok(ref mut imds) = imds.inner().lock() {
        metrics = Some(imds.get_fast_metrics());
    } else {
        metrics = None;
    }

    if let Some(metrics) = metrics {
        for metric in metrics.iter() {
            ret.push_str(&format!("{}\n", metric.as_text()))
        }
    }

    ret.push_str("\n");

    return Some(ret);
}

#[get("/")]
pub fn metrics(
    imds: &State<Arc<Mutex<utilities::imds::IMDS>>>,
    entity_metrics: &State<Arc<Mutex<EntityMetricsStore>>>,
    polling: &State<Arc<crate::collectors::PollingControl>>,
) -> Option<String> {
    // Polling paused: emit nothing switch-derived (IMDS counters, entity/PoE/QoS
    // samples, per-device SNMP poll counters) so Prometheus stops scraping stale
    // switch series. The /perf endpoint (process counters) stays live.
    if !polling.enabled() {
        return Some("\n".to_string());
    }
    let mut ret : String = String::new();
    let metrics : Option<Vec<models::metrics::LabeledMetric>>;

    // Take only a cheap owned snapshot under the global IMDS lock, then build
    // the LabeledMetric list outside it — the build no longer blocks pollers
    // for the whole scrape (PERF.md #2). The timed section is just the snapshot,
    // which is what still contends with the pollers.
    let snapshot = if let Ok(ref imds) = imds.inner().lock() {
        let build_start = std::time::Instant::now();
        let snap = imds.metrics_snapshot();
        utilities::perfstats::PERF.record_metrics_build(build_start.elapsed());
        Some(snap)
    } else {
        None
    };
    metrics = snapshot.map(|s| utilities::imds::IMDS::metrics_from(&s));

    if let Some(metrics) = metrics {
        for metric in metrics.iter() {
            ret.push_str(&format!("{}\n", metric.as_text()))
        }
    }

    // Entity sensor + STP metrics from the in-process entitypoller collector.
    if let Ok(ref store) = entity_metrics.inner().lock() {
        ret.push_str(&store.render());
    }

    // Per-device/query SNMP poll counters (all collectors, via the SnmpSource
    // seam) for attributing and rating the polling load.
    ret.push_str(&utilities::pollstats::render());

    ret.push_str("\n");

    return Some(ret);
}

// Lock-free hot-path performance counters (see utilities::perfstats). Separate
// from the metric endpoints above so the perf harness can scrape it cheaply
// without touching the IMDS lock.
#[get("/perf")]
pub fn metrics_perf() -> String {
    let mut s = utilities::perfstats::PERF.render();
    // Adaptive per-device timeout aggregates (embedded mode). Low cardinality:
    // fleet-wide counts + the max effective timeout, no per-device series. Read
    // from the embedded client's process-global registry at scrape time.
    let (elevated, dead, max_eff_ms) = crate::snmp::embedded::adapt_aggregate();
    s.push_str(&format!(
        "# HELP jaspy_perf_snmp_devices_elevated Devices whose adaptive SNMP timeout is above the floor\n# TYPE jaspy_perf_snmp_devices_elevated gauge\njaspy_perf_snmp_devices_elevated {}\n",
        elevated
    ));
    s.push_str(&format!(
        "# HELP jaspy_perf_snmp_devices_dead Devices collapsed to fast-fail (unresponsive at the max timeout)\n# TYPE jaspy_perf_snmp_devices_dead gauge\njaspy_perf_snmp_devices_dead {}\n",
        dead
    ));
    s.push_str(&format!(
        "# HELP jaspy_perf_snmp_effective_timeout_max_ms Largest adaptive SNMP timeout across devices (ms)\n# TYPE jaspy_perf_snmp_effective_timeout_max_ms gauge\njaspy_perf_snmp_effective_timeout_max_ms {}\n",
        max_eff_ms
    ));
    s
}

// By-name dump of the entire live adaptive-SNMP registry (every device, primary +
// per-VLAN sessions) — the named companion to the aggregate gauges above, for
// CLI/ops inspection. Empty in snmpbot mode. Lock-free like /perf.
#[get("/snmp-adaptive")]
pub fn snmp_adaptive() -> Json<Vec<models::json::ApiSnmpAdaptiveDevice>> {
    let mut devices: Vec<models::json::ApiSnmpAdaptiveDevice> = Vec::new();
    // all_adaptive_sessions() is sorted by fqdn then vlan, so consecutive entries
    // for one device group together.
    for (fqdn, snap) in crate::snmp::embedded::all_adaptive_sessions() {
        let session = models::json::ApiSnmpSession {
            vlan: snap.vlan,
            port: snap.port,
            effective_timeout_ms: snap.effective_ms,
            ewma_latency_ms: snap.ewma_ms,
            consec_timeouts: snap.consec_timeouts,
            dead: snap.dead,
            status: snap.status.to_string(),
        };
        match devices.last_mut() {
            Some(d) if d.fqdn == fqdn => d.sessions.push(session),
            _ => devices.push(models::json::ApiSnmpAdaptiveDevice { fqdn, sessions: vec![session] }),
        }
    }
    Json(devices)
}
