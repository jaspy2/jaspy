use std::collections::HashMap;

pub struct InterfaceMetrics {
    expiry: f64,

    name: String,
    neighbors: bool,
    speed_override: Option<u64>,

    in_octets: Option<u64>,
    out_octets: Option<u64>,
    in_packets: Option<u64>,
    out_packets: Option<u64>,
    in_errors: Option<u64>,
    out_errors: Option<u64>,
    up: Option<bool>,
    speed: Option<u64>,
}

pub struct DeviceMetrics {
    expiry: f64,

    fqdn: String,
    name: String,
    dns_domain: String,

    up: Option<bool>,

    interfaces: HashMap<i64, InterfaceMetrics>,
}

pub struct Metrics {
    devices: HashMap<String, DeviceMetrics>,
}