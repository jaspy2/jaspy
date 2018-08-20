extern crate rocket_contrib;
use std::sync::{Arc, Mutex};
use utilities;
use rocket::State;
use models;

#[get("/fast")]
fn metrics_fast(imds: State<Arc<Mutex<utilities::imds::IMDS>>>) -> Option<String> {
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
fn metrics(imds: State<Arc<Mutex<utilities::imds::IMDS>>>) -> Option<String> {
    let mut ret : String = String::new();
    let metrics : Option<Vec<models::metrics::LabeledMetric>>;

    if let Ok(ref mut imds) = imds.inner().lock() {
        metrics = Some(imds.get_metrics());
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