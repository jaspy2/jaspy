use std::collections::{HashMap, HashSet};

#[derive(Clone)]
pub struct InterfaceMetrics {
    pub expiry: f64,

    pub name: String,
    pub neighbors: bool,
    pub interface_type: String,
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
    Int64(i64),
    Uint64(u64),
}

pub struct LabeledMetric {
    pub name: String,
    pub labels: HashMap<String, String>,
    pub value: MetricValue,
}

impl LabeledMetric {
    pub fn new(name: &String, value: MetricValue, labels: &HashMap<String,String>) -> LabeledMetric {
        return LabeledMetric {
            name: name.clone(),
            value: value,
            labels: labels.clone(),
        }
    }

    pub fn as_text(self: &LabeledMetric) -> String {
        let mut label_data : Vec<String> = Vec::new();
        for (label, value) in self.labels.iter() {
            label_data.push(format!("{}=\"{}\"", label, value));
        }
        let labeltext = label_data.join(",");
        let body;
        match self.value {
            MetricValue::Int64(value) => {
                body = format!("{}{{{}}} {}", self.name, labeltext, value);
            },
            MetricValue::Uint64(value) => {
                body = format!("{}{{{}}} {}", self.name, labeltext, value);
            }
        }
        return format!("{}", body);
    }
}