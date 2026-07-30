use std::collections::{HashMap};

#[derive(Clone)]
pub struct InterfaceMetrics {
    pub name: String,
    pub neighbors: bool,
    pub interface_type: String,
    pub last_report: u64,
    pub speed_override: Option<i32>,

    pub in_octets: Option<u64>,
    pub out_octets: Option<u64>,
    pub in_unicast_packets: Option<u64>,
    pub in_multicast_packets: Option<u64>,
    pub in_broadcast_packets: Option<u64>,
    pub out_unicast_packets: Option<u64>,
    pub out_multicast_packets: Option<u64>,
    pub out_broadcast_packets: Option<u64>,
    pub in_errors: Option<u64>,
    pub out_errors: Option<u64>,
    pub out_discards: Option<u64>,
    pub up: Option<bool>,
    pub speed: Option<i32>,

    // ifAdminStatus (None = not yet learned). Lets the UI tell an admin-shut
    // port apart from a link-down one.
    pub admin_up: Option<bool>,
    // Authoritative Cisco err-disable state: whether the switch has
    // error-disabled the port, the cause (enum name, e.g. "bpduGuard"), and the
    // seconds until auto-recovery. Populated from CISCO-ERR-DISABLE-MIB.
    pub err_disabled: bool,
    pub err_disable_cause: Option<String>,
    pub err_disable_recover_secs: Option<i32>,

    pub counter_violations: u64,
}

impl InterfaceMetrics {
    // Make an interface look never-polled: clear all accumulated counters,
    // operational state, and the last-report timestamp, leaving only stable
    // identity (name/type/neighbors/speed_override) intact. Used when the
    // physical device behind an interface is replaced (see IMDS::refresh_device
    // base_mac reset): the new hardware's fresh, lower counters would otherwise
    // be rejected as regressions by validate_counters, and a stale last_report
    // would make the first post-swap health sample span a huge interval. Single
    // source of truth so a newly added counter field can't be forgotten by an
    // ad-hoc reset.
    pub fn reset_counters(&mut self) {
        self.last_report = 0;
        self.in_octets = None;
        self.out_octets = None;
        self.in_unicast_packets = None;
        self.in_multicast_packets = None;
        self.in_broadcast_packets = None;
        self.out_unicast_packets = None;
        self.out_multicast_packets = None;
        self.out_broadcast_packets = None;
        self.in_errors = None;
        self.out_errors = None;
        self.out_discards = None;
        self.up = None;
        self.speed = None;
        self.admin_up = None;
        self.err_disabled = false;
        self.err_disable_cause = None;
        self.err_disable_recover_secs = None;
        self.counter_violations = 0;
    }
}

#[derive(Clone)]
pub struct DeviceMetrics {
    pub fqdn: String,

    pub hostname: String,

    // Chassis base MAC last seen for this device (from discovery). A change
    // means the physical device was swapped behind the same fqdn/ip; IMDS uses
    // it to reset stale per-interface counters. None = not yet learned.
    pub base_mac: Option<String>,

    pub up: Option<bool>,

    pub last_report: u64,

    // Timestamp (msecs) of the last interface report received for this device
    // (SNMP poll or trap); 0 = never. Surfaced as secondsSinceLastPoll in the UI.
    pub last_poll: u64,

    pub interfaces: HashMap<i32, InterfaceMetrics>,
}

#[derive(Clone)]
pub struct Metrics {
    pub devices: HashMap<String, DeviceMetrics>,
}

pub enum MetricValue {
    Int64(i64),
    Uint64(u64),
    Float64(f64),
}

impl MetricValue {
    pub fn as_f64(&self) -> f64 {
        match self {
            MetricValue::Int64(v) => *v as f64,
            MetricValue::Uint64(v) => *v as f64,
            MetricValue::Float64(v) => *v,
        }
    }

    pub fn as_i64(&self) -> i64 {
        match self {
            MetricValue::Int64(v) => *v,
            MetricValue::Uint64(v) => *v as i64,
            MetricValue::Float64(v) => *v as i64,
        }
    }
}

pub struct LabeledMetric {
    pub name: String,
    pub labels: HashMap<String, String>,
    pub value: MetricValue,
    pub timestamp: u64,
}

impl LabeledMetric {
    pub fn new(name: &String, value: MetricValue, labels: &HashMap<String,String>, timestamp: u64) -> LabeledMetric {
        return LabeledMetric {
            name: name.clone(),
            value: value,
            labels: labels.clone(),
            timestamp: timestamp,
        }
    }

    #[cfg(test)]
    pub fn from_parts(name: &str, value: MetricValue, labels: &[(&str, &str)], timestamp: u64) -> LabeledMetric {
        let labels: HashMap<String, String> = labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        LabeledMetric::new(&name.to_string(), value, &labels, timestamp)
    }

    pub fn as_text(self: &LabeledMetric) -> String {
        let mut label_data : Vec<String> = Vec::new();
        for (label, value) in self.labels.iter() {
            label_data.push(format!("{}=\"{}\"", label, value));
        }
        // Deterministic output regardless of HashMap iteration order.
        label_data.sort();
        let labeltext = label_data.join(",");
        let body;
        match self.value {
            MetricValue::Int64(value) => {
                body = format!("{}{{{}}} {} {}", self.name, labeltext, value, self.timestamp);
            },
            MetricValue::Uint64(value) => {
                body = format!("{}{{{}}} {} {}", self.name, labeltext, value, self.timestamp);
            },
            MetricValue::Float64(value) => {
                body = format!("{}{{{}}} {} {}", self.name, labeltext, value, self.timestamp);
            }
        }
        return format!("{}", body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_text_int64() {
        let m = LabeledMetric::from_parts("jaspy_device_up", MetricValue::Int64(-1), &[("fqdn", "sw1.example.com")], 1000);
        assert_eq!(m.as_text(), "jaspy_device_up{fqdn=\"sw1.example.com\"} -1 1000");
    }

    #[test]
    fn as_text_uint64() {
        let m = LabeledMetric::from_parts("jaspy_octets", MetricValue::Uint64(u64::MAX), &[("direction", "rx")], 5);
        assert_eq!(m.as_text(), format!("jaspy_octets{{direction=\"rx\"}} {} 5", u64::MAX));
    }

    #[test]
    fn as_text_float64() {
        let m = LabeledMetric::from_parts("jaspy_sensor", MetricValue::Float64(21.5), &[("sensor", "temp")], 7);
        assert_eq!(m.as_text(), "jaspy_sensor{sensor=\"temp\"} 21.5 7");
    }

    #[test]
    fn as_text_no_labels() {
        let m = LabeledMetric::from_parts("jaspy_thing", MetricValue::Uint64(1), &[], 1);
        assert_eq!(m.as_text(), "jaspy_thing{} 1 1");
    }

    #[test]
    fn value_accessors_cast_all_variants() {
        assert_eq!(MetricValue::Int64(-3).as_f64(), -3.0);
        assert_eq!(MetricValue::Uint64(7).as_f64(), 7.0);
        assert_eq!(MetricValue::Float64(4.5).as_f64(), 4.5);
        assert_eq!(MetricValue::Int64(-3).as_i64(), -3);
        assert_eq!(MetricValue::Uint64(7).as_i64(), 7);
        assert_eq!(MetricValue::Float64(4.9).as_i64(), 4);
    }

    #[test]
    fn as_text_sorts_labels() {
        let m = LabeledMetric::from_parts(
            "jaspy_interface_up",
            MetricValue::Int64(1),
            &[("neighbors", "yes"), ("fqdn", "sw1.example.com"), ("name", "Ethernet1/1")],
            42,
        );
        assert_eq!(
            m.as_text(),
            "jaspy_interface_up{fqdn=\"sw1.example.com\",name=\"Ethernet1/1\",neighbors=\"yes\"} 1 42"
        );
    }
}
