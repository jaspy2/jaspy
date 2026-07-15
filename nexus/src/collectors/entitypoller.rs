// In-process entity/STP sensor collector (formerly the standalone Go
// `jaspy-entitypoller` binary). Ported near-verbatim from
// entitypoller/src/entitypoller.go.
//
// Unlike poller/pinger it does not touch IMDS or the DB report path: it is a
// stateless Prometheus exporter. Each cycle it enumerates monitored devices,
// fans out one worker thread per device (mirroring the Go `runOnce`+WaitGroup),
// polls snmpbot for entity sensors and per-VLAN STP state, and stores the
// rendered samples in a shared `EntityMetricsStore`. The `/dev/metrics` route
// appends that store to nexus's single Prometheus endpoint, replacing the old
// dedicated :8098 listener.
extern crate reqwest;
extern crate serde_json;

use crate::collectors::poller::{SNMPBotResponse, SNMPBotResultEntryObjectValue};
use crate::db;
use crate::models::metrics::{LabeledMetric, MetricValue};
use crate::utilities::tools;
use rand::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::{atomic, Arc, Mutex};
use std::thread;
use std::time;

// ---------------------------------------------------------------------------
// Shared metrics store: latest rendered samples per device, replaced each poll.
// ---------------------------------------------------------------------------

pub struct EntityMetricsStore {
    devices: HashMap<String, Vec<LabeledMetric>>,
}

impl EntityMetricsStore {
    pub fn new() -> EntityMetricsStore {
        EntityMetricsStore { devices: HashMap::new() }
    }

    fn replace_device(&mut self, fqdn: String, metrics: Vec<LabeledMetric>) {
        self.devices.insert(fqdn, metrics);
    }

    // Drop metrics for devices no longer monitored (the Go version leaked these).
    fn retain(&mut self, keep: &HashSet<String>) {
        self.devices.retain(|fqdn, _| keep.contains(fqdn));
    }

    // Prometheus text for every stored device, one metric per line.
    pub fn render(&self) -> String {
        let mut ret = String::new();
        for metrics in self.devices.values() {
            for metric in metrics.iter() {
                ret.push_str(&format!("{}\n", metric.as_text()));
            }
        }
        return ret;
    }

    // Latest results for one device as structured JSON DTOs for /api/v1.
    // Unknown fqdn and not-yet-polled both yield empty vectors.
    pub fn device_entity(&self, fqdn: &str) -> crate::models::json::ApiDeviceEntity {
        use crate::models::json::{ApiDeviceEntity, ApiEntitySensor, ApiStpPort};

        let label = |metric: &LabeledMetric, key: &str| -> String {
            metric.labels.get(key).cloned().unwrap_or_default()
        };
        // "" (sensors) / "UNKNOWN" (STP) mean the poller could not associate
        // the row with an interface; "0" likewise for interface ids.
        let opt_name = |name: String| -> Option<String> {
            if name.is_empty() || name == "UNKNOWN" { None } else { Some(name) }
        };
        let opt_id = |id: &str| -> Option<i64> {
            match id.parse::<i64>() {
                Ok(0) | Err(_) => None,
                Ok(v) => Some(v),
            }
        };

        let mut sensors: Vec<ApiEntitySensor> = Vec::new();
        let mut stp: std::collections::BTreeMap<(i64, i64), ApiStpPort> = std::collections::BTreeMap::new();

        for metric in self.devices.get(fqdn).map(|m| m.iter()).into_iter().flatten() {
            if metric.name == "jaspy_sensors" {
                sensors.push(ApiEntitySensor {
                    sensor_id: label(metric, "sensor_id").parse().unwrap_or(0),
                    name: label(metric, "sensor_name"),
                    description: label(metric, "sensor_description"),
                    value: metric.value.as_f64(),
                    value_type: label(metric, "value_type"),
                    interface_name: opt_name(label(metric, "interface_name")),
                    interface_id: opt_id(&label(metric, "interface_id")),
                    timestamp: metric.timestamp,
                });
            } else if let Some(key) = metric.name.strip_prefix("jaspy_stp_") {
                let vlan: i64 = match label(metric, "vlan").parse() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let stp_port_id: i64 = match label(metric, "stp_port_id").parse() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let port = stp.entry((vlan, stp_port_id)).or_insert_with(|| ApiStpPort {
                    vlan: vlan,
                    stp_port_id: stp_port_id,
                    interface_name: opt_name(label(metric, "interface_name")),
                    interface_id: opt_id(&label(metric, "interface_id")),
                    role: "unknown".to_string(),
                    state: "unknown".to_string(),
                    enabled: None,
                    designated_cost: 0,
                    path_cost: 0,
                    priority: 0,
                    forward_transitions: 0,
                    timestamp: 0,
                });
                let value = metric.value.as_i64();
                match key {
                    "port_role" => port.role = rstp_port_role_text(value).to_string(),
                    "port_state" => port.state = stp_port_state_text(value).to_string(),
                    "port_enabled" => port.enabled = stp_port_enable_bool(value),
                    "port_designated_cost" => port.designated_cost = value,
                    "port_path_cost" => port.path_cost = value,
                    "port_priority" => port.priority = value,
                    "port_forward_transitions" => port.forward_transitions = value,
                    _ => {}
                }
                port.timestamp = std::cmp::max(port.timestamp, metric.timestamp);
            }
        }

        sensors.sort_by(|a, b| a.name.cmp(&b.name).then(a.sensor_id.cmp(&b.sensor_id)));
        let mut stp: Vec<ApiStpPort> = stp.into_values().collect();
        stp.sort_by(|a, b| {
            a.vlan.cmp(&b.vlan)
                .then_with(|| a.interface_name.cmp(&b.interface_name))
                .then_with(|| a.stp_port_id.cmp(&b.stp_port_id))
        });

        ApiDeviceEntity { sensors: sensors, stp: stp }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metric(name: &str, value: i64) -> LabeledMetric {
        LabeledMetric::from_parts(name, MetricValue::Int64(value), &[("fqdn", "sw1.example.com")], 1)
    }

    #[test]
    fn rstp_role_numeric_all_variants() {
        assert_eq!(rstp_port_role_numeric("disabled"), 1);
        assert_eq!(rstp_port_role_numeric("root"), 2);
        assert_eq!(rstp_port_role_numeric("designated"), 3);
        assert_eq!(rstp_port_role_numeric("alternate"), 4);
        assert_eq!(rstp_port_role_numeric("backUp"), 5);
        assert_eq!(rstp_port_role_numeric("boundary"), 6);
        assert_eq!(rstp_port_role_numeric("master"), 7);
        assert_eq!(rstp_port_role_numeric("something-new"), 0);
    }

    #[test]
    fn stp_state_numeric_all_variants() {
        assert_eq!(stp_port_state_numeric("disabled"), 1);
        assert_eq!(stp_port_state_numeric("blocking"), 2);
        assert_eq!(stp_port_state_numeric("listening"), 3);
        assert_eq!(stp_port_state_numeric("learning"), 4);
        assert_eq!(stp_port_state_numeric("forwarding"), 5);
        assert_eq!(stp_port_state_numeric("broken"), 6);
        assert_eq!(stp_port_state_numeric(""), 0);
    }

    #[test]
    fn stp_enable_numeric_all_variants() {
        assert_eq!(stp_port_enable_numeric("enabled"), 1);
        assert_eq!(stp_port_enable_numeric("disabled"), 2);
        assert_eq!(stp_port_enable_numeric(""), 0);
    }

    fn objects(json: &str) -> HashMap<String, SNMPBotResultEntryObjectValue> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn obj_f64_accepts_numbers_rejects_strings() {
        let objs = objects(r#"{"uint": 42, "float": 4.5, "str": "42"}"#);
        assert_eq!(obj_f64(&objs, "uint"), Some(42.0));
        assert_eq!(obj_f64(&objs, "float"), Some(4.5));
        assert_eq!(obj_f64(&objs, "str"), None);
        assert_eq!(obj_f64(&objs, "missing"), None);
    }

    #[test]
    fn obj_i64_accepts_numbers_rejects_strings() {
        let objs = objects(r#"{"uint": 42, "float": 4.9, "str": "42"}"#);
        assert_eq!(obj_i64(&objs, "uint"), Some(42));
        assert_eq!(obj_i64(&objs, "float"), Some(4));
        assert_eq!(obj_i64(&objs, "str"), None);
    }

    #[test]
    fn obj_str_accepts_strings_only() {
        let objs = objects(r#"{"uint": 42, "str": "up"}"#);
        assert_eq!(obj_str(&objs, "str"), Some("up".to_string()));
        assert_eq!(obj_str(&objs, "uint"), None);
        assert_eq!(obj_str(&objs, "missing"), None);
    }

    #[test]
    fn store_replace_and_render() {
        let mut store = EntityMetricsStore::new();
        store.replace_device("sw1.example.com".to_string(), vec![metric("jaspy_stp_port_state", 5)]);
        assert_eq!(store.render(), "jaspy_stp_port_state{fqdn=\"sw1.example.com\"} 5 1\n");

        // Replacing a device overwrites its previous metrics wholesale.
        store.replace_device("sw1.example.com".to_string(), vec![metric("jaspy_stp_port_state", 2)]);
        assert_eq!(store.render(), "jaspy_stp_port_state{fqdn=\"sw1.example.com\"} 2 1\n");
    }

    #[test]
    fn store_retain_drops_unmonitored_devices() {
        let mut store = EntityMetricsStore::new();
        store.replace_device("keep.example.com".to_string(), vec![metric("jaspy_sensors", 1)]);
        store.replace_device("drop.example.com".to_string(), vec![metric("jaspy_sensors", 2)]);
        let keep: HashSet<String> = vec!["keep.example.com".to_string()].into_iter().collect();
        store.retain(&keep);
        let rendered = store.render();
        assert!(rendered.contains("jaspy_sensors"));
        assert_eq!(rendered.lines().count(), 1);
    }

    #[test]
    fn render_empty_store_is_empty() {
        assert_eq!(EntityMetricsStore::new().render(), "");
    }

    // --- inverse enum maps stay in sync with the encoders ---

    #[test]
    fn rstp_role_text_roundtrip() {
        for role in ["disabled", "root", "designated", "alternate", "backUp", "boundary", "master"] {
            assert_eq!(rstp_port_role_text(rstp_port_role_numeric(role)), role);
        }
        assert_eq!(rstp_port_role_text(0), "unknown");
        assert_eq!(rstp_port_role_text(99), "unknown");
    }

    #[test]
    fn stp_state_text_roundtrip() {
        for state in ["disabled", "blocking", "listening", "learning", "forwarding", "broken"] {
            assert_eq!(stp_port_state_text(stp_port_state_numeric(state)), state);
        }
        assert_eq!(stp_port_state_text(0), "unknown");
    }

    #[test]
    fn stp_enable_bool_all_variants() {
        assert_eq!(stp_port_enable_bool(1), Some(true));
        assert_eq!(stp_port_enable_bool(2), Some(false));
        assert_eq!(stp_port_enable_bool(0), None);
    }

    // --- device_entity: store -> API DTO conversion ---

    const DEV: &str = "sw1.example.com";

    fn sensor_metric(name: &str, value: f64, iface: &str, iface_id: &str, ts: u64) -> LabeledMetric {
        LabeledMetric::from_parts("jaspy_sensors", MetricValue::Float64(value), &[
            ("hostname", "sw1"), ("fqdn", DEV),
            ("sensor_id", "1006"), ("sensor_name", name), ("sensor_description", "Temperature Sensor"),
            ("value_type", "celsius"), ("interface_name", iface), ("interface_id", iface_id),
        ], ts)
    }

    fn stp_metrics(vlan: &str, port: &str, values: &[(&str, i64)], ts: u64) -> Vec<LabeledMetric> {
        values.iter().map(|(key, value)| {
            LabeledMetric::from_parts(&format!("jaspy_stp_{}", key), MetricValue::Int64(*value), &[
                ("hostname", "sw1"), ("fqdn", DEV), ("vlan", vlan), ("stp_port_id", port),
                ("interface_name", "GigabitEthernet0/1"), ("interface_id", "10101"),
            ], ts)
        }).collect()
    }

    // Mirrors the e2e fixture: forwarding/designated/enabled, costs 4/19,
    // priority 128, 2 transitions.
    fn full_stp_port(vlan: &str, port: &str, ts: u64) -> Vec<LabeledMetric> {
        stp_metrics(vlan, port, &[
            ("port_role", 3), ("port_state", 5), ("port_enabled", 1),
            ("port_designated_cost", 4), ("port_path_cost", 19),
            ("port_priority", 128), ("port_forward_transitions", 2),
        ], ts)
    }

    #[test]
    fn device_entity_unknown_fqdn_is_empty() {
        let entity = EntityMetricsStore::new().device_entity("ghost.example.com");
        assert!(entity.sensors.is_empty());
        assert!(entity.stp.is_empty());
    }

    #[test]
    fn device_entity_sensor_row() {
        let mut store = EntityMetricsStore::new();
        store.replace_device(DEV.to_string(), vec![
            sensor_metric("GigabitEthernet0/1 Module Temperature Sensor", 45.0, "GigabitEthernet0/1", "10101", 1234),
        ]);
        let entity = store.device_entity(DEV);
        assert_eq!(entity.sensors.len(), 1);
        let sensor = &entity.sensors[0];
        assert_eq!(sensor.sensor_id, 1006);
        assert_eq!(sensor.name, "GigabitEthernet0/1 Module Temperature Sensor");
        assert_eq!(sensor.description, "Temperature Sensor");
        assert_eq!(sensor.value, 45.0);
        assert_eq!(sensor.value_type, "celsius");
        assert_eq!(sensor.interface_name.as_deref(), Some("GigabitEthernet0/1"));
        assert_eq!(sensor.interface_id, Some(10101));
        assert_eq!(sensor.timestamp, 1234);
        assert!(entity.stp.is_empty());
    }

    #[test]
    fn device_entity_sensor_without_interface_association() {
        let mut store = EntityMetricsStore::new();
        store.replace_device(DEV.to_string(), vec![sensor_metric("PSU 1", 12.1, "", "0", 1)]);
        let sensor = &store.device_entity(DEV).sensors[0];
        assert_eq!(sensor.interface_name, None);
        assert_eq!(sensor.interface_id, None);
    }

    #[test]
    fn device_entity_stp_grouping_merges_metrics_per_vlan_port() {
        let mut store = EntityMetricsStore::new();
        let mut metrics = full_stp_port("100", "5", 1000);
        metrics.extend(full_stp_port("200", "5", 2000));
        store.replace_device(DEV.to_string(), metrics);

        let entity = store.device_entity(DEV);
        assert!(entity.sensors.is_empty());
        assert_eq!(entity.stp.len(), 2);
        let port = &entity.stp[0];
        assert_eq!((port.vlan, port.stp_port_id), (100, 5));
        assert_eq!(port.role, "designated");
        assert_eq!(port.state, "forwarding");
        assert_eq!(port.enabled, Some(true));
        assert_eq!(port.designated_cost, 4);
        assert_eq!(port.path_cost, 19);
        assert_eq!(port.priority, 128);
        assert_eq!(port.forward_transitions, 2);
        assert_eq!(port.interface_name.as_deref(), Some("GigabitEthernet0/1"));
        assert_eq!(port.interface_id, Some(10101));
        assert_eq!(port.timestamp, 1000);
        assert_eq!(entity.stp[1].vlan, 200);
    }

    #[test]
    fn device_entity_decodes_unknown_numerics_and_keeps_defaults() {
        let mut store = EntityMetricsStore::new();
        // Partial row: only role/state/enabled, all with the "unknown" encoding;
        // "UNKNOWN"/"0" interface labels mean no association.
        let metrics = stp_metrics("100", "5", &[("port_role", 0), ("port_state", 0), ("port_enabled", 0)], 1)
            .into_iter()
            .map(|mut m| {
                m.labels.insert("interface_name".to_string(), "UNKNOWN".to_string());
                m.labels.insert("interface_id".to_string(), "0".to_string());
                m
            })
            .collect();
        store.replace_device(DEV.to_string(), metrics);

        let port = &store.device_entity(DEV).stp[0];
        assert_eq!(port.role, "unknown");
        assert_eq!(port.state, "unknown");
        assert_eq!(port.enabled, None);
        assert_eq!(port.interface_name, None);
        assert_eq!(port.interface_id, None);
        // Missing metrics keep zero defaults (partial-cycle degradation).
        assert_eq!(port.designated_cost, 0);
        assert_eq!(port.path_cost, 0);
    }

    #[test]
    fn device_entity_sort_orders() {
        let mut store = EntityMetricsStore::new();
        let mut metrics = vec![
            sensor_metric("Zeta Sensor", 1.0, "", "0", 1),
            sensor_metric("Alpha Sensor", 2.0, "", "0", 1),
        ];
        metrics.extend(full_stp_port("200", "1", 1));
        metrics.extend(full_stp_port("100", "2", 1));
        metrics.extend(full_stp_port("100", "1", 1));
        store.replace_device(DEV.to_string(), metrics);

        let entity = store.device_entity(DEV);
        let sensor_names: Vec<&str> = entity.sensors.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(sensor_names, vec!["Alpha Sensor", "Zeta Sensor"]);
        let stp_keys: Vec<(i64, i64)> = entity.stp.iter().map(|p| (p.vlan, p.stp_port_id)).collect();
        assert_eq!(stp_keys, vec![(100, 1), (100, 2), (200, 1)]);
    }

    #[test]
    fn device_entity_ignores_other_devices() {
        let mut store = EntityMetricsStore::new();
        store.replace_device("other.example.com".to_string(), vec![sensor_metric("S", 1.0, "", "0", 1)]);
        assert!(store.device_entity(DEV).sensors.is_empty());
    }
}

// ---------------------------------------------------------------------------
// Per-device metadata loaded from the DB (replaces the Go /dev/device API calls)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct EntityDevice {
    hostname: String,
    fqdn: String,
    community: String,
    // Interface lookup keyed by both interface name and description, mapping to
    // (interface name, interface id) for sensor->interface association.
    interfaces: HashMap<String, (String, i32)>,
}

fn load_devices(pool: &db::Pool) -> Vec<EntityDevice> {
    let mut devices: Vec<EntityDevice> = Vec::new();
    if let Ok(mut conn) = pool.get() {
        for device in crate::models::dbo::Device::monitored(&mut *conn).iter() {
            let community = match device.snmp_community {
                Some(ref c) => c.clone(),
                None => continue,
            };
            let fqdn = format!("{}.{}", device.name, device.dns_domain);
            let mut interfaces: HashMap<String, (String, i32)> = HashMap::new();
            for interface in device.interfaces(&mut *conn).iter() {
                let value = (interface.name.clone(), interface.id);
                interfaces.insert(interface.name.clone(), value.clone());
                if let Some(ref description) = interface.description {
                    interfaces.insert(description.clone(), value.clone());
                }
            }
            devices.push(EntityDevice {
                hostname: device.name.clone(),
                fqdn: fqdn,
                community: community,
                interfaces: interfaces,
            });
        }
    } else {
        println!("[entitypoller] failed to acquire db connection for device listing");
    }
    return devices;
}

// ---------------------------------------------------------------------------
// snmpbot access + value helpers
// ---------------------------------------------------------------------------

// entitypoller addresses hosts inline (community@fqdn), unlike the poller's
// `?snmp=community@fqdn` query form; snmpbot supports both. Preserved for a
// drop-in match against production snmpbot.
pub(crate) fn fetch_table(snmpbot_url: &String, host: &String, table: &str) -> Option<SNMPBotResponse> {
    let url = format!("{}/api/hosts/{}/tables/{}", snmpbot_url, host, table);
    let response = match reqwest::blocking::get(&url) {
        Ok(r) => r,
        Err(_) => return None,
    };
    if !response.status().is_success() {
        println!("[{}] snmpbot returned ({}) for {}, skipping", host, response.status(), table);
        return None;
    }
    let body = match response.text() {
        Ok(body) => body,
        Err(what) => {
            println!("[{}] error reading response for {}: {}", host, table, what);
            return None;
        }
    };
    match serde_json::from_str(&body) {
        Ok(parsed) => Some(parsed),
        Err(what) => {
            println!("[{}] error parsing json for {}: {} (body: {:.200})", host, table, what, body);
            None
        }
    }
}

fn obj_f64(objects: &HashMap<String, SNMPBotResultEntryObjectValue>, key: &str) -> Option<f64> {
    match objects.get(key) {
        Some(SNMPBotResultEntryObjectValue::Uint64(v)) => Some(*v as f64),
        Some(SNMPBotResultEntryObjectValue::Float64(v)) => Some(*v),
        _ => None,
    }
}

pub(crate) fn obj_i64(objects: &HashMap<String, SNMPBotResultEntryObjectValue>, key: &str) -> Option<i64> {
    match objects.get(key) {
        Some(SNMPBotResultEntryObjectValue::Uint64(v)) => Some(*v as i64),
        Some(SNMPBotResultEntryObjectValue::Float64(v)) => Some(*v as i64),
        _ => None,
    }
}

pub(crate) fn obj_str(objects: &HashMap<String, SNMPBotResultEntryObjectValue>, key: &str) -> Option<String> {
    match objects.get(key) {
        Some(SNMPBotResultEntryObjectValue::Str(v)) => Some(v.clone()),
        _ => None,
    }
}

// --- STP enum encodings (entitypoller.go:80-148) ---

fn rstp_port_role_numeric(role: &str) -> i64 {
    match role {
        "disabled" => 1,
        "root" => 2,
        "designated" => 3,
        "alternate" => 4,
        "backUp" => 5,
        "boundary" => 6,
        "master" => 7,
        _ => 0,
    }
}

fn stp_port_state_numeric(state: &str) -> i64 {
    match state {
        "disabled" => 1,
        "blocking" => 2,
        "listening" => 3,
        "learning" => 4,
        "forwarding" => 5,
        "broken" => 6,
        _ => 0,
    }
}

fn stp_port_enable_numeric(enable: &str) -> i64 {
    match enable {
        "enabled" => 1,
        "disabled" => 2,
        _ => 0,
    }
}

// Inverse mappings, used when serving the stored (numeric) metrics back as
// structured JSON for the web UI. Keep in sync with the encoders above.

fn rstp_port_role_text(role: i64) -> &'static str {
    match role {
        1 => "disabled",
        2 => "root",
        3 => "designated",
        4 => "alternate",
        5 => "backUp",
        6 => "boundary",
        7 => "master",
        _ => "unknown",
    }
}

fn stp_port_state_text(state: i64) -> &'static str {
    match state {
        1 => "disabled",
        2 => "blocking",
        3 => "listening",
        4 => "learning",
        5 => "forwarding",
        6 => "broken",
        _ => "unknown",
    }
}

fn stp_port_enable_bool(enable: i64) -> Option<bool> {
    match enable {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Sensor polling (ENTITY-MIB / ENTITY-SENSOR-MIB / CISCO-ENTITY-SENSOR-MIB)
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct EntityIdentity {
    id: i64,
    name: String,
    description: String,
}

fn get_entities(snmpbot_url: &String, device: &EntityDevice, out: &mut Vec<LabeledMetric>) {
    let host = format!("{}@{}", device.community, device.fqdn);
    let phys = match fetch_table(snmpbot_url, &host, "ENTITY-MIB::entPhysicalTable") {
        Some(t) => t,
        None => return,
    };

    let mut identities: HashMap<i64, EntityIdentity> = HashMap::new();
    for entry in phys.entries.iter() {
        let id = match entry.index.get("ENTITY-MIB::entPhysicalIndex") {
            Some(id) => *id,
            None => continue,
        };
        identities.insert(id, EntityIdentity {
            id: id,
            name: obj_str(&entry.objects, "ENTITY-MIB::entPhysicalName").unwrap_or_default(),
            description: obj_str(&entry.objects, "ENTITY-MIB::entPhysicalDescr").unwrap_or_default(),
        });
    }

    // Standard ENTITY-SENSOR-MIB then CISCO-ENTITY-SENSOR-MIB.
    get_entities_by_physical_index(
        snmpbot_url, device, &host, &identities, out,
        "ENTITY-SENSOR-MIB::entPhySensorValue",
        "ENTITY-SENSOR-MIB::entPhySensorScale",
        "ENTITY-SENSOR-MIB::entPhySensorPrecision",
        "ENTITY-SENSOR-MIB::entPhySensorType",
        "ENTITY-SENSOR-MIB::entPhySensorTable",
    );
    get_entities_by_physical_index(
        snmpbot_url, device, &host, &identities, out,
        "CISCO-ENTITY-SENSOR-MIB::entSensorValue",
        "CISCO-ENTITY-SENSOR-MIB::entSensorScale",
        "CISCO-ENTITY-SENSOR-MIB::entSensorPrecision",
        "CISCO-ENTITY-SENSOR-MIB::entSensorType",
        "CISCO-ENTITY-SENSOR-MIB::entSensorValueTable",
    );
}

fn get_entities_by_physical_index(
    snmpbot_url: &String,
    device: &EntityDevice,
    host: &String,
    identities: &HashMap<i64, EntityIdentity>,
    out: &mut Vec<LabeledMetric>,
    value_field: &str,
    scale_field: &str,
    precision_field: &str,
    value_type_field: &str,
    table_field: &str,
) {
    let table = match fetch_table(snmpbot_url, host, table_field) {
        Some(t) => t,
        None => return,
    };
    let timestamp = tools::get_time_msecs();

    for entry in table.entries.iter() {
        let entity_id = match entry.index.get("ENTITY-MIB::entPhysicalIndex") {
            Some(id) => *id,
            None => continue,
        };
        let identity = identities.get(&entity_id).cloned().unwrap_or_default();

        // Skip entries missing any required field, matching the Go nil checks.
        let mut value = match obj_f64(&entry.objects, value_field) {
            Some(v) => v,
            None => continue,
        };
        let value_type = match obj_str(&entry.objects, value_type_field) {
            Some(v) => v,
            None => continue,
        };
        let scale = match obj_str(&entry.objects, scale_field) {
            Some(v) => v,
            None => continue,
        };
        let precision = match obj_f64(&entry.objects, precision_field) {
            Some(v) => v,
            None => continue,
        };

        if scale == "milli" {
            value = value / 1000.0;
        }
        if precision > 0.0 {
            value = value / 10f64.powf(precision);
        }

        // Sensor->interface association: first whitespace token of the sensor
        // name looked up in the interface map (keyed by name and description).
        let name_to_check = identity.name.split(' ').next().unwrap_or("");
        let (interface_name, interface_id) = match device.interfaces.get(name_to_check) {
            Some((name, id)) => (name.clone(), id.to_string()),
            None => (String::new(), "0".to_string()),
        };

        let mut labels: HashMap<String, String> = HashMap::new();
        labels.insert("hostname".to_string(), device.hostname.clone());
        labels.insert("fqdn".to_string(), device.fqdn.clone());
        labels.insert("sensor_id".to_string(), identity.id.to_string());
        labels.insert("sensor_name".to_string(), identity.name.clone());
        labels.insert("sensor_description".to_string(), identity.description.clone());
        labels.insert("value_type".to_string(), value_type);
        labels.insert("interface_name".to_string(), interface_name);
        labels.insert("interface_id".to_string(), interface_id);

        out.push(LabeledMetric::new(
            &"jaspy_sensors".to_string(),
            MetricValue::Float64(value),
            &labels,
            timestamp,
        ));
    }
}

// ---------------------------------------------------------------------------
// STP polling (CISCO-STP-EXTENSIONS-MIB + BRIDGE-MIB, per VLAN)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct StpPortInfo {
    role: String,
    interface_id: String,   // real ifIndex (from dot1dBasePortIfIndex), "0" if unknown
    interface_name: String, // ifDescr of that ifIndex, "UNKNOWN" if unknown
}

fn get_stp(snmpbot_url: &String, device: &EntityDevice, out: &mut Vec<LabeledMetric>) {
    let host = format!("{}@{}", device.community, device.fqdn);
    let role_table = match fetch_table(snmpbot_url, &host, "CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleTable") {
        Some(t) => t,
        None => return,
    };

    // vlan -> bridge port -> StpPortInfo
    let mut vlans: HashMap<i64, HashMap<i64, StpPortInfo>> = HashMap::new();
    for entry in role_table.entries.iter() {
        let vlan = match entry.index.get("CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleInstanceIndex") {
            Some(v) => *v,
            None => continue,
        };
        let ifidx = match entry.index.get("CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRolePortIndex") {
            Some(v) => *v,
            None => continue,
        };
        let status = match obj_str(&entry.objects, "CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleValue") {
            Some(v) => v,
            None => continue,
        };
        vlans.entry(vlan).or_insert_with(HashMap::new).insert(ifidx, StpPortInfo {
            role: status,
            interface_id: "0".to_string(),
            interface_name: "UNKNOWN".to_string(),
        });
    }

    // ifTable (base community) for real ifIndex -> ifDescr names.
    let iftable = match fetch_table(snmpbot_url, &host, "IF-MIB::ifTable") {
        Some(t) => t,
        None => return,
    };
    let mut interfaces: HashMap<i64, String> = HashMap::new();
    for entry in iftable.entries.iter() {
        let ifidx = match entry.index.get("IF-MIB::ifIndex") {
            Some(v) => *v,
            None => continue,
        };
        if let Some(descr) = obj_str(&entry.objects, "IF-MIB::ifDescr") {
            interfaces.insert(ifidx, descr);
        }
    }

    let timestamp = tools::get_time_msecs();
    for (vlan, ports) in vlans.iter_mut() {
        let per_vlan_host = format!("{}@{}@{}", device.community, vlan, device.fqdn);

        // dot1dBasePortTable: bridge port -> real ifIndex (+ resolve ifDescr).
        let base_table = match fetch_table(snmpbot_url, &per_vlan_host, "BRIDGE-MIB::dot1dBasePortTable") {
            Some(t) => t,
            None => return,
        };
        for entry in base_table.entries.iter() {
            let bridge_port = match entry.index.get("BRIDGE-MIB::dot1dBasePort") {
                Some(v) => *v,
                None => continue,
            };
            let real_ifidx = match obj_i64(&entry.objects, "BRIDGE-MIB::dot1dBasePortIfIndex") {
                Some(v) => v,
                None => continue,
            };
            if let Some(port) = ports.get_mut(&bridge_port) {
                port.interface_id = real_ifidx.to_string();
                if let Some(descr) = interfaces.get(&real_ifidx) {
                    port.interface_name = descr.clone();
                }
            }
        }

        // dot1dStpPortTable: cost/priority/transitions/state/enable per port.
        let stp_table = match fetch_table(snmpbot_url, &per_vlan_host, "BRIDGE-MIB::dot1dStpPortTable") {
            Some(t) => t,
            None => return,
        };
        for entry in stp_table.entries.iter() {
            let bridge_port = match entry.index.get("BRIDGE-MIB::dot1dStpPort") {
                Some(v) => *v,
                None => continue,
            };
            let port = match ports.get(&bridge_port) {
                Some(p) => p,
                None => continue,
            };

            let designated_cost = obj_i64(&entry.objects, "BRIDGE-MIB::dot1dStpPortDesignatedCost").unwrap_or(0);
            let path_cost = obj_i64(&entry.objects, "BRIDGE-MIB::dot1dStpPortPathCost").unwrap_or(0);
            let priority = obj_i64(&entry.objects, "BRIDGE-MIB::dot1dStpPortPriority").unwrap_or(0);
            let forward_transitions = obj_i64(&entry.objects, "BRIDGE-MIB::dot1dStpPortForwardTransitions").unwrap_or(0);
            let enable = obj_str(&entry.objects, "BRIDGE-MIB::dot1dStpPortEnable").unwrap_or_default();
            let state = obj_str(&entry.objects, "BRIDGE-MIB::dot1dStpPortState").unwrap_or_default();

            push_stp_metric(out, device, port, *vlan, bridge_port, "port_designated_cost", designated_cost, timestamp);
            push_stp_metric(out, device, port, *vlan, bridge_port, "port_path_cost", path_cost, timestamp);
            push_stp_metric(out, device, port, *vlan, bridge_port, "port_priority", priority, timestamp);
            push_stp_metric(out, device, port, *vlan, bridge_port, "port_forward_transitions", forward_transitions, timestamp);
            push_stp_metric(out, device, port, *vlan, bridge_port, "port_enabled", stp_port_enable_numeric(&enable), timestamp);
            push_stp_metric(out, device, port, *vlan, bridge_port, "port_role", rstp_port_role_numeric(&port.role), timestamp);
            push_stp_metric(out, device, port, *vlan, bridge_port, "port_state", stp_port_state_numeric(&state), timestamp);
        }
    }
}

fn push_stp_metric(out: &mut Vec<LabeledMetric>, device: &EntityDevice, port: &StpPortInfo, vlan: i64, bridge_port: i64, key: &str, value: i64, timestamp: u64) {
    let mut labels: HashMap<String, String> = HashMap::new();
    labels.insert("hostname".to_string(), device.hostname.clone());
    labels.insert("fqdn".to_string(), device.fqdn.clone());
    labels.insert("vlan".to_string(), vlan.to_string());
    labels.insert("stp_port_id".to_string(), bridge_port.to_string());
    labels.insert("interface_name".to_string(), port.interface_name.clone());
    labels.insert("interface_id".to_string(), port.interface_id.clone());
    out.push(LabeledMetric::new(
        &format!("jaspy_stp_{}", key),
        MetricValue::Int64(value),
        &labels,
        timestamp,
    ));
}

// ---------------------------------------------------------------------------
// Supervisor
// ---------------------------------------------------------------------------

pub(crate) fn interruptible_sleep(msecs: u64, running: &Arc<atomic::AtomicBool>) {
    let mut slept = 0;
    while slept < msecs && running.load(atomic::Ordering::Relaxed) {
        let chunk = std::cmp::min(250, msecs - slept);
        thread::sleep(time::Duration::from_millis(chunk));
        slept += chunk;
    }
}

pub fn run(snmpbot_url: String, interval_msecs: u64, disable_sensors: bool, disable_stp: bool, store: Arc<Mutex<EntityMetricsStore>>, running: Arc<atomic::AtomicBool>) {
    println!("[entitypoller] starting in-process collector (snmpbot={}, interval_msecs={}, sensors={}, stp={})",
        snmpbot_url, interval_msecs, !disable_sensors, !disable_stp);
    let pool = db::connect();
    let no_jitter = std::env::var("JASPY_POLLER_NO_JITTER").map(|v| v == "1" || v == "true").unwrap_or(false);

    while running.load(atomic::Ordering::Relaxed) {
        let cycle_start = tools::get_time_msecs();
        let devices = load_devices(&pool);

        // Drop metrics for devices that are no longer monitored.
        let keep: HashSet<String> = devices.iter().map(|d| d.fqdn.clone()).collect();
        if let Ok(mut store) = store.lock() {
            store.retain(&keep);
        }

        // One worker thread per device, joined at a barrier (Go runOnce+WaitGroup).
        let mut handles = Vec::new();
        for device in devices.into_iter() {
            let snmpbot_url = snmpbot_url.clone();
            let store = store.clone();
            handles.push(thread::spawn(move || {
                if !no_jitter {
                    let sleep = thread_rng().gen_range(0.0, (interval_msecs / 2) as f64);
                    thread::sleep(time::Duration::from_millis(sleep as u64));
                }
                let mut metrics: Vec<LabeledMetric> = Vec::new();
                if !disable_sensors {
                    get_entities(&snmpbot_url, &device, &mut metrics);
                }
                if !disable_stp {
                    get_stp(&snmpbot_url, &device, &mut metrics);
                }
                if let Ok(mut store) = store.lock() {
                    store.replace_device(device.fqdn.clone(), metrics);
                }
            }));
        }
        for handle in handles {
            let _ = handle.join();
        }

        let elapsed = tools::get_time_msecs() - cycle_start;
        if elapsed < interval_msecs {
            interruptible_sleep(interval_msecs - elapsed, &running);
        }
    }
    println!("[entitypoller] collector stopped");
}
