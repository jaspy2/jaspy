use std::collections::{HashMap, HashSet};

pub struct InterfaceMetrics {
    pub expiry: f64,

    pub name: String,
    pub neighbors: bool,
    pub speed_override: Option<i32>,

    pub in_octets: Option<u64>,
    pub out_octets: Option<u64>,
    pub in_packets: Option<u64>,
    pub out_packets: Option<u64>,
    pub in_errors: Option<u64>,
    pub out_errors: Option<u64>,
    pub up: Option<bool>,
    pub speed: Option<i32>,
}

pub struct DeviceMetrics {
    pub expiry: f64,

    pub fqdn: String,

    pub up: Option<bool>,

    pub interfaces: HashMap<i32, InterfaceMetrics>,
}

pub struct Metrics {
    pub devices: HashMap<String, DeviceMetrics>,
}

pub struct DeviceMetricRefreshCacheMiss {
    pub miss_set: HashSet<String>,
}

impl DeviceMetricRefreshCacheMiss {
    pub fn new() -> DeviceMetricRefreshCacheMiss {
        let dmrcm = DeviceMetricRefreshCacheMiss {
            miss_set: HashSet::new()
        };
        return dmrcm;
    }
}

pub enum MetricValue {
    Float64(f64),
    Int64(i64),
}

pub struct LabeledMetric {
    name: String,
    labels: HashMap<String, String>,
    value: MetricValue,
}

impl LabeledMetric {
    pub fn new(name: &String, value: MetricValue, labels: &HashMap<String,String>) -> LabeledMetric {
        return LabeledMetric {
            name: name.clone(),
            value: value,
            labels: labels.clone(),
        }
    }
}