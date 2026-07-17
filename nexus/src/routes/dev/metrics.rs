use std::sync::{Arc, Mutex};
use crate::utilities;
use crate::collectors::entitypoller::EntityMetricsStore;
use rocket::State;
use crate::models;
use rocket::get;

// TODO: GH#9 Move everything to v1 API
#[get("/fast")]
pub fn metrics_fast(imds: &State<Arc<Mutex<utilities::imds::IMDS>>>) -> Option<String> {
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
pub fn metrics(imds: &State<Arc<Mutex<utilities::imds::IMDS>>>, entity_metrics: &State<Arc<Mutex<EntityMetricsStore>>>) -> Option<String> {
    let mut ret : String = String::new();
    let metrics : Option<Vec<models::metrics::LabeledMetric>>;

    if let Ok(ref mut imds) = imds.inner().lock() {
        // Time the metric-Vec build: this runs under the global IMDS lock and
        // blocks every poller for its duration (PERF.md #2).
        let build_start = std::time::Instant::now();
        let built = imds.get_metrics();
        utilities::perfstats::PERF.record_metrics_build(build_start.elapsed());
        metrics = Some(built);
    } else {
        metrics = None;
    }

    if let Some(metrics) = metrics {
        for metric in metrics.iter() {
            ret.push_str(&format!("{}\n", metric.as_text()))
        }
    }

    // Entity sensor + STP metrics from the in-process entitypoller collector.
    if let Ok(ref store) = entity_metrics.inner().lock() {
        ret.push_str(&store.render());
    }

    ret.push_str("\n");

    return Some(ret);
}

// Lock-free hot-path performance counters (see utilities::perfstats). Separate
// from the metric endpoints above so the perf harness can scrape it cheaply
// without touching the IMDS lock.
#[get("/perf")]
pub fn metrics_perf() -> String {
    utilities::perfstats::PERF.render()
}
