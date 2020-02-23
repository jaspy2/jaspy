extern crate rocket_contrib;
use std::sync::{Arc, Mutex};
use crate::utilities;
use rocket::State;
use crate::models;
use rocket::get;

// TODO: GH#9 Move everything to v1 API
#[get("/fast")]
pub fn metrics_fast(imds: State<Arc<Mutex<utilities::imds::IMDS>>>) -> Option<String> {
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
pub fn metrics(imds: State<Arc<Mutex<utilities::imds::IMDS>>>) -> Option<String> {
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
