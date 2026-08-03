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
extern crate serde_json;

use crate::collectors::poller::{SNMPBotResponse, SNMPBotResultEntryObjectValue};
use crate::snmp::{HostSpec, SnmpSource};
use crate::collectors::vendor::{self, Vendor};
use crate::db;
use crate::models::metrics::{LabeledMetric, MetricValue};
use crate::utilities::tools;
use std::collections::{HashMap, HashSet};
use std::sync::{atomic, Arc, Mutex};
use std::thread;
use std::time;

// ---------------------------------------------------------------------------
// Shared metrics store: latest rendered samples per device, replaced each poll.
// ---------------------------------------------------------------------------

// One device's poll result: the raw Prometheus samples plus the structured
// DTO decoded from them once at write time — the /api/v1 endpoints (polled
// every 30s by the UI, for every device at once) then only clone under the
// store lock instead of re-decoding per request.
struct DeviceEntity {
    metrics: Vec<LabeledMetric>,
    entity: crate::models::json::ApiDeviceEntity,
}

pub struct EntityMetricsStore {
    devices: HashMap<String, DeviceEntity>,
    // Live per-interface media/form-factor overlay (db interface id -> media),
    // derived from ENTITY-MIB alongside the sensor poll. Kept separate from
    // DeviceEntity so replace_device stays a pure metrics sink. Served to
    // /api/v1 as an overlay on the persisted discovery baseline.
    media: HashMap<String, HashMap<i32, String>>,
    // Live per-interface PoE state (db interface id -> InterfacePoe) and the
    // switch-wide PSE budget per device, from POWER-ETHERNET-MIB (+ Cisco ext).
    // In-memory only, like media: PoE is dynamic and re-polled each cycle.
    poe: HashMap<String, HashMap<i32, crate::collectors::poe::InterfacePoe>>,
    poe_budget: HashMap<String, Vec<crate::collectors::poe::PoeBudget>>,
    // Live Cisco QoS (policy-map) counters per device, one entry per class in
    // each applied service-policy (CISCO-CLASS-BASED-QOS-MIB). In-memory only
    // like poe: dynamic, re-polled each cycle, absent for non-QoS devices.
    qos: HashMap<String, Vec<crate::collectors::qos::QosClass>>,
}

impl EntityMetricsStore {
    pub fn new() -> EntityMetricsStore {
        EntityMetricsStore {
            devices: HashMap::new(),
            media: HashMap::new(),
            poe: HashMap::new(),
            poe_budget: HashMap::new(),
            qos: HashMap::new(),
        }
    }

    fn replace_device(&mut self, fqdn: String, metrics: Vec<LabeledMetric>) {
        let entity = decode_entity(&metrics);
        self.devices.insert(fqdn, DeviceEntity { metrics: metrics, entity: entity });
    }

    // Replace the media overlay for one device (empty => forget it, so a device
    // that stops answering ENTITY-MIB falls back to the DB baseline).
    fn set_media(&mut self, fqdn: String, media: HashMap<i32, String>) {
        if media.is_empty() {
            self.media.remove(&fqdn);
        } else {
            self.media.insert(fqdn, media);
        }
    }

    // Live media overlay for one device (db interface id -> media). Empty when
    // the entitypoller hasn't (or can't) classified this device's ports.
    pub fn media_for(&self, fqdn: &str) -> HashMap<i32, String> {
        self.media.get(fqdn).cloned().unwrap_or_default()
    }

    // Replace the per-interface PoE overlay for one device (empty => forget it,
    // so a device that stops answering PoE MIBs falls back to no overlay).
    fn set_poe(&mut self, fqdn: String, poe: HashMap<i32, crate::collectors::poe::InterfacePoe>) {
        if poe.is_empty() {
            self.poe.remove(&fqdn);
        } else {
            self.poe.insert(fqdn, poe);
        }
    }

    // Replace the switch-wide PSE budget for one device (empty => forget it).
    fn set_poe_budget(&mut self, fqdn: String, budget: Vec<crate::collectors::poe::PoeBudget>) {
        if budget.is_empty() {
            self.poe_budget.remove(&fqdn);
        } else {
            self.poe_budget.insert(fqdn, budget);
        }
    }

    // Live PoE overlay for one device (db interface id -> InterfacePoe). Empty
    // when the device exposes no PoE MIBs (or hasn't been polled yet).
    pub fn poe_for(&self, fqdn: &str) -> HashMap<i32, crate::collectors::poe::InterfacePoe> {
        self.poe.get(fqdn).cloned().unwrap_or_default()
    }

    // Switch-wide PSE budget for one device (one entry per PSE group).
    pub fn poe_budget_for(&self, fqdn: &str) -> Vec<crate::collectors::poe::PoeBudget> {
        self.poe_budget.get(fqdn).cloned().unwrap_or_default()
    }

    // Replace the QoS overlay for one device (empty => forget it, so a device
    // that stops answering CBQoS falls back to no overlay).
    fn set_qos(&mut self, fqdn: String, qos: Vec<crate::collectors::qos::QosClass>) {
        if qos.is_empty() {
            self.qos.remove(&fqdn);
        } else {
            self.qos.insert(fqdn, qos);
        }
    }

    // Every device's PSE budget at once, for the fleet-wide Issues scan
    // (mirrors network_stp). Devices without PoE are omitted.
    pub fn network_poe_budget(&self) -> HashMap<String, Vec<crate::collectors::poe::PoeBudget>> {
        self.poe_budget.clone()
    }

    // Drop metrics for devices no longer monitored (the Go version leaked these).
    fn retain(&mut self, keep: &HashSet<String>) {
        self.devices.retain(|fqdn, _| keep.contains(fqdn));
        self.media.retain(|fqdn, _| keep.contains(fqdn));
        self.poe.retain(|fqdn, _| keep.contains(fqdn));
        self.poe_budget.retain(|fqdn, _| keep.contains(fqdn));
        self.qos.retain(|fqdn, _| keep.contains(fqdn));
    }

    // Prometheus text for every stored device, one metric per line.
    pub fn render(&self) -> String {
        let mut ret = String::new();
        for device in self.devices.values() {
            for metric in device.metrics.iter() {
                ret.push_str(&format!("{}\n", metric.as_text()));
            }
        }
        return ret;
    }

    // Latest results for one device as structured JSON DTOs for /api/v1.
    // Unknown fqdn and not-yet-polled both yield empty vectors.
    pub fn device_entity(&self, fqdn: &str) -> crate::models::json::ApiDeviceEntity {
        let mut entity = match self.devices.get(fqdn) {
            Some(device) => device.entity.clone(),
            None => crate::models::json::ApiDeviceEntity {
                sensors: Vec::new(),
                stp: Vec::new(),
                stp_bridges: Vec::new(),
                qos: Vec::new(),
            },
        };
        // QoS is stored structured (like poe), separate from the metrics-decoded
        // sensors/STP; attach it here so /api/v1 gets it in one payload.
        entity.qos = self.qos.get(fqdn).map(|q| qos_to_api(q)).unwrap_or_default();
        entity
    }

    // Every device's STP data at once, for the network-wide tree computation
    // (GET /api/v1/stp*). Devices without STP rows are omitted from the maps.
    pub fn network_stp(&self) -> (
        HashMap<String, Vec<crate::models::json::ApiStpPort>>,
        HashMap<String, Vec<crate::models::json::ApiStpBridge>>,
    ) {
        let mut ports = HashMap::new();
        let mut bridges = HashMap::new();
        for (fqdn, device) in self.devices.iter() {
            if !device.entity.stp.is_empty() {
                ports.insert(fqdn.clone(), device.entity.stp.clone());
            }
            if !device.entity.stp_bridges.is_empty() {
                bridges.insert(fqdn.clone(), device.entity.stp_bridges.clone());
            }
        }
        (ports, bridges)
    }
}

// Store rows -> structured DTOs; shared by device_entity and network_stp.
fn decode_entity(metrics: &[LabeledMetric]) -> crate::models::json::ApiDeviceEntity {
    use crate::models::json::{ApiDeviceEntity, ApiEntitySensor, ApiStpBridge, ApiStpPort};

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
    let mut bridges: std::collections::BTreeMap<i64, ApiStpBridge> = std::collections::BTreeMap::new();

    {
        for metric in metrics.iter() {
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
            } else if let Some(key) = metric.name.strip_prefix("jaspy_stp_bridge_") {
                // Must match before the jaspy_stp_ port branch (same prefix).
                let vlan: i64 = match label(metric, "vlan").parse() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let bridge = bridges.entry(vlan).or_insert_with(|| ApiStpBridge {
                    vlan: vlan,
                    root_priority: None,
                    root_mac: None,
                    root_cost: None,
                    root_port: None,
                    root_port_interface_name: None,
                    topology_changes: None,
                    time_since_topology_change_secs: None,
                    timestamp: 0,
                });
                let value = metric.value.as_i64();
                match key {
                    "root_priority" => {
                        bridge.root_priority = Some(value);
                        let mac = label(metric, "root_mac");
                        if !mac.is_empty() {
                            bridge.root_mac = Some(mac);
                        }
                    }
                    "root_cost" => bridge.root_cost = Some(value),
                    "root_port" => bridge.root_port = Some(value),
                    "topology_changes" => bridge.topology_changes = Some(value),
                    "time_since_topology_change" => bridge.time_since_topology_change_secs = Some(value),
                    _ => {}
                }
                bridge.timestamp = std::cmp::max(bridge.timestamp, metric.timestamp);
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
    }

    sensors.sort_by(|a, b| a.name.cmp(&b.name).then(a.sensor_id.cmp(&b.sensor_id)));
    let mut stp: Vec<ApiStpPort> = stp.into_values().collect();
    stp.sort_by(|a, b| {
        a.vlan.cmp(&b.vlan)
            .then_with(|| a.interface_name.cmp(&b.interface_name))
            .then_with(|| a.stp_port_id.cmp(&b.stp_port_id))
    });

    // Resolve each vlan's reported root port (a bridge port number) to the
    // interface name via that vlan's port rows.
    let mut stp_bridges: Vec<ApiStpBridge> = bridges.into_values().collect();
    for bridge in stp_bridges.iter_mut() {
        if let Some(root_port) = bridge.root_port {
            bridge.root_port_interface_name = stp
                .iter()
                .find(|p| p.vlan == bridge.vlan && p.stp_port_id == root_port)
                .and_then(|p| p.interface_name.clone());
        }
    }

    // qos is attached separately by device_entity() from the structured store.
    ApiDeviceEntity { sensors: sensors, stp: stp, stp_bridges: stp_bridges, qos: Vec::new() }
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

    // --- bridge scalar metrics -> ApiStpBridge ---

    fn bridge_metric(key: &str, value: i64, vlan: &str, extra: &[(&str, &str)], ts: u64) -> LabeledMetric {
        let mut labels: Vec<(&str, &str)> = vec![("hostname", "sw1"), ("fqdn", DEV), ("vlan", vlan)];
        labels.extend_from_slice(extra);
        LabeledMetric::from_parts(&format!("jaspy_stp_bridge_{}", key), MetricValue::Int64(value), &labels, ts)
    }

    #[test]
    fn device_entity_decodes_bridge_scalars() {
        let mut store = EntityMetricsStore::new();
        let mut metrics = vec![
            bridge_metric("root_priority", 33068, "100", &[("root_mac", "70:10:6f:63:f2:70")], 10),
            bridge_metric("root_cost", 20000, "100", &[], 11),
            bridge_metric("root_port", 5, "100", &[], 12),
            bridge_metric("topology_changes", 9, "100", &[], 13),
            bridge_metric("time_since_topology_change", 22979, "100", &[], 14),
        ];
        // A port row on the same vlan whose stp_port_id matches root_port, so
        // rootPortInterfaceName resolves.
        metrics.extend(full_stp_port("100", "5", 1));
        store.replace_device(DEV.to_string(), metrics);

        let entity = store.device_entity(DEV);
        assert_eq!(entity.stp_bridges.len(), 1);
        let bridge = &entity.stp_bridges[0];
        assert_eq!(bridge.vlan, 100);
        assert_eq!(bridge.root_priority, Some(33068));
        assert_eq!(bridge.root_mac.as_deref(), Some("70:10:6f:63:f2:70"));
        assert_eq!(bridge.root_cost, Some(20000));
        assert_eq!(bridge.root_port, Some(5));
        assert_eq!(bridge.root_port_interface_name.as_deref(), Some("GigabitEthernet0/1"));
        assert_eq!(bridge.topology_changes, Some(9));
        assert_eq!(bridge.time_since_topology_change_secs, Some(22979));
        assert_eq!(bridge.timestamp, 14);
        // The port rows are unaffected by the bridge branch.
        assert_eq!(entity.stp.len(), 1);
    }

    #[test]
    fn device_entity_partial_bridge_scalars_stay_none() {
        let mut store = EntityMetricsStore::new();
        store.replace_device(DEV.to_string(), vec![bridge_metric("root_cost", 4, "100", &[], 1)]);
        let bridge = &store.device_entity(DEV).stp_bridges[0];
        assert_eq!(bridge.root_cost, Some(4));
        assert_eq!(bridge.root_priority, None);
        assert_eq!(bridge.root_mac, None);
        assert_eq!(bridge.root_port_interface_name, None);
    }

    #[test]
    fn network_stp_returns_every_device() {
        let mut store = EntityMetricsStore::new();
        let mut metrics = full_stp_port("100", "5", 1);
        metrics.push(bridge_metric("root_cost", 4, "100", &[], 1));
        store.replace_device(DEV.to_string(), metrics);
        store.replace_device("other.example.com".to_string(), vec![sensor_metric("S", 1.0, "", "0", 1)]);

        let (ports, bridges) = store.network_stp();
        assert_eq!(ports.len(), 1, "sensor-only device contributes no STP");
        assert_eq!(ports[DEV].len(), 1);
        assert_eq!(bridges[DEV][0].root_cost, Some(4));
    }

    // --- HP RPVST+ fallback decode ---

    const RPVST_ROLES: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/hpicfrpvstroletable.json"));
    const RPVST_STATES: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/hpicfrpvststatetable.json"));
    const RPVST_COSTS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/hpicfrpvstcosttable.json"));
    const RPVST_VLANS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/hpicfrpvstvlantable.json"));

    fn rpvst_fixture_metrics() -> Vec<LabeledMetric> {
        let parse = |s: &str| -> SNMPBotResponse { serde_json::from_str(s).unwrap() };
        let device = EntityDevice {
            hostname: "sw1".to_string(),
            fqdn: DEV.to_string(),
            community: "testcomm".to_string(),
            vendor: Vendor::HpProcurve,
            interfaces: HashMap::new(),
        };
        let mut interfaces = HashMap::new();
        for port in [5i64, 6, 7, 8] {
            interfaces.insert(port, format!("{}", port));
        }
        rpvst_metrics(
            &device,
            &interfaces,
            &parse(RPVST_ROLES),
            Some(&parse(RPVST_STATES)),
            Some(&parse(RPVST_COSTS)),
            Some(&parse(RPVST_VLANS)),
            77,
        )
    }

    #[test]
    fn rpvst_ports_decode_through_device_entity() {
        let mut store = EntityMetricsStore::new();
        store.replace_device(DEV.to_string(), rpvst_fixture_metrics());
        let entity = store.device_entity(DEV);

        // Port 8 (out-of-enum numeric role) is skipped: 4 rows on vlan 100
        // minus 1 skipped, plus 1 on vlan 200.
        assert_eq!(entity.stp.len(), 4);
        let root = entity.stp.iter().find(|p| p.vlan == 100 && p.stp_port_id == 5).unwrap();
        assert_eq!(root.role, "root");
        assert_eq!(root.state, "forwarding");
        assert_eq!(root.path_cost, 20000);
        assert_eq!(root.interface_id, Some(5));
        assert_eq!(root.interface_name.as_deref(), Some("5"));
        let blocked = entity.stp.iter().find(|p| p.vlan == 100 && p.stp_port_id == 7).unwrap();
        assert_eq!(blocked.role, "alternate");
        assert_eq!(blocked.state, "blocking");
        assert!(!entity.stp.iter().any(|p| p.stp_port_id == 8), "numeric role rows skipped");
    }

    #[test]
    fn rpvst_bridge_scalars_decode() {
        let mut store = EntityMetricsStore::new();
        store.replace_device(DEV.to_string(), rpvst_fixture_metrics());
        let entity = store.device_entity(DEV);

        assert_eq!(entity.stp_bridges.len(), 2);
        let v100 = entity.stp_bridges.iter().find(|b| b.vlan == 100).unwrap();
        assert_eq!(v100.root_priority, Some(32768));
        assert_eq!(v100.root_mac.as_deref(), Some("70:10:6f:63:f2:70"));
        assert_eq!(v100.root_cost, Some(20000));
        assert_eq!(v100.root_port, Some(5));
        assert_eq!(v100.root_port_interface_name.as_deref(), Some("5"));
        // Fractional TimeTicks (Float64) truncate to whole seconds.
        assert_eq!(v100.time_since_topology_change_secs, Some(18158911));
        assert_eq!(v100.topology_changes, Some(3));
        // The root-itself vlan: port 0 resolves no interface name.
        let v200 = entity.stp_bridges.iter().find(|b| b.vlan == 200).unwrap();
        assert_eq!(v200.root_cost, Some(0));
        assert_eq!(v200.root_mac.as_deref(), Some("aa:bb:cc:dd:ee:01"));
        assert_eq!(v200.root_port_interface_name, None);
        assert_eq!(v200.time_since_topology_change_secs, Some(2297973));
    }

    // Gap-1 seam: the root MAC an HP ProCurve reports via HP-ICF-RPVST-MIB
    // (not BRIDGE-MIB) must feed root-mismatch detection just like a Cisco's.
    // Composes the real RPVST decode with build_stp_tree rather than trusting
    // each layer in isolation.
    #[test]
    fn rpvst_reported_root_drives_root_mismatch() {
        use crate::models::json::{ApiStpPort, WeathermapBase};
        use crate::utilities::stp::{build_stp_tree, StpInputs};

        let mut store = EntityMetricsStore::new();
        store.replace_device(DEV.to_string(), rpvst_fixture_metrics());
        let (mut ports, bridges) = store.network_stp();
        // The HP switch decoded a root port on vlan 100, so it is a non-root
        // node. Add a monitored bridge that IS the elected root (no root port),
        // whose base MAC differs from the 70:10:6f:63:f2:70 the HP reports.
        ports.insert(
            "root.example.com".to_string(),
            vec![ApiStpPort {
                vlan: 100,
                stp_port_id: 1,
                interface_name: Some("1".to_string()),
                interface_id: Some(1),
                role: "designated".to_string(),
                state: "forwarding".to_string(),
                enabled: Some(true),
                designated_cost: 0,
                path_cost: 0,
                priority: 128,
                forward_transitions: 1,
                timestamp: 1,
            }],
        );
        let mut base_macs = HashMap::new();
        base_macs.insert("root.example.com".to_string(), Some("aa:bb:cc:dd:ee:01".to_string()));
        base_macs.insert(DEV.to_string(), Some("70:10:6f:63:f2:70".to_string()));

        let tree = build_stp_tree(
            &StpInputs {
                ports: &ports,
                bridges: &bridges,
                base_macs: &base_macs,
                topology: &WeathermapBase { devices: HashMap::new() },
                lag_members: &HashMap::new(),
                vlan_members: &HashMap::new(),
            },
            100,
        );
        assert_eq!(tree.roots, vec!["root.example.com".to_string()]);
        let hp = tree.nodes.iter().find(|n| n.fqdn == DEV).unwrap();
        assert!(hp.root_mismatch, "HP-reported root must be compared to the elected root");
        let detail = hp.root_mismatch_detail.as_ref().unwrap();
        assert_eq!(detail.reported_root_mac.as_deref(), Some("70:10:6f:63:f2:70"));
        assert_eq!(detail.computed_root_fqdn, "root.example.com");
    }

    // --- bridge scalars via the jaspyStpBridgeTable view ---

    const BRIDGE_SCALARS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/jaspystpbridgetable.json"));

    fn test_device() -> EntityDevice {
        EntityDevice {
            hostname: "sw1".to_string(),
            fqdn: DEV.to_string(),
            community: "testcomm".to_string(),
            vendor: Vendor::Cisco,
            interfaces: HashMap::new(),
        }
    }

    #[test]
    fn bridge_scalar_metrics_decodes_all_five() {
        let table: SNMPBotResponse = serde_json::from_str(BRIDGE_SCALARS).unwrap();
        let mut store = EntityMetricsStore::new();
        store.replace_device(DEV.to_string(), bridge_scalar_metrics(&test_device(), 100, &table.entries[0].objects, 7));

        let bridge = &store.device_entity(DEV).stp_bridges[0];
        assert_eq!(bridge.vlan, 100);
        assert_eq!(bridge.root_priority, Some(33068));
        assert_eq!(bridge.root_mac.as_deref(), Some("70:10:6f:63:f2:70"));
        assert_eq!(bridge.root_cost, Some(20000));
        assert_eq!(bridge.root_port, Some(5));
        assert_eq!(bridge.topology_changes, Some(9));
        assert_eq!(bridge.time_since_topology_change_secs, Some(2297973));
    }

    #[test]
    fn bridge_scalar_metrics_skips_missing_and_malformed() {
        // Unparseable bridge id and absent scalars: only root_cost decodes.
        let entry: crate::collectors::poller::SNMPBotResultEntry = serde_json::from_str(r#"{
            "HostID": "h",
            "Index": {"BRIDGE-MIB::jaspyStpBridgeInstance": 0},
            "Objects": {
                "BRIDGE-MIB::dot1dStpDesignatedRoot": "zz zz",
                "BRIDGE-MIB::dot1dStpRootCost": 4
            }
        }"#).unwrap();
        let metrics = bridge_scalar_metrics(&test_device(), 100, &entry.objects, 7);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].name, "jaspy_stp_bridge_root_cost");
    }

    #[test]
    fn bridge_metric_renders_exact_text() {
        let metric = bridge_metric("root_priority", 33068, "100", &[("root_mac", "aa:bb:cc:dd:ee:ff")], 7);
        assert_eq!(
            metric.as_text(),
            "jaspy_stp_bridge_root_priority{fqdn=\"sw1.example.com\",hostname=\"sw1\",root_mac=\"aa:bb:cc:dd:ee:ff\",vlan=\"100\"} 33068 7"
        );
    }

    // --- PoE metrics ---

    fn poe_test_device() -> EntityDevice {
        // Interface map keyed by name and description, both -> (name, id), as
        // load_devices builds it. Interface id 501 resolves to a name; others
        // don't (exercises the empty-name fallback).
        let mut interfaces: HashMap<String, (String, i32)> = HashMap::new();
        interfaces.insert("GigabitEthernet0/1".to_string(), ("GigabitEthernet0/1".to_string(), 501));
        interfaces.insert("uplink to core".to_string(), ("GigabitEthernet0/1".to_string(), 501));
        EntityDevice {
            hostname: "sw1".to_string(),
            fqdn: DEV.to_string(),
            community: "testcomm".to_string(),
            vendor: Vendor::Cisco,
            interfaces,
        }
    }

    fn find<'a>(metrics: &'a [LabeledMetric], name: &str) -> &'a LabeledMetric {
        metrics.iter().find(|m| m.name == name).expect(name)
    }

    #[test]
    fn poe_metrics_per_port_cisco_full() {
        use crate::collectors::poe::{InterfacePoe, PoeStatus};
        let mut poe = HashMap::new();
        poe.insert(501, InterfacePoe {
            admin_enabled: true,
            status: PoeStatus::Delivering,
            class: Some(4),
            power_mw: Some(4578),
            allocated_mw: Some(15400),
            max_drawn_mw: Some(5229),
            priority: Some("low".to_string()),
            ent_phy_index: Some(1005),
        });
        let metrics = poe_metrics(&poe_test_device(), &poe, &[], 7);

        // Label set + ordering locked via the always-present admin gauge.
        assert_eq!(
            find(&metrics, "jaspy_poe_port_admin_enabled").as_text(),
            "jaspy_poe_port_admin_enabled{fqdn=\"sw1.example.com\",hostname=\"sw1\",interface_id=\"501\",name=\"GigabitEthernet0/1\"} 1 7"
        );
        assert_eq!(find(&metrics, "jaspy_poe_port_status").value.as_i64(), 3);
        assert_eq!(find(&metrics, "jaspy_poe_port_class").value.as_i64(), 4);
        // Cisco milliwatts scaled to watts.
        assert_eq!(find(&metrics, "jaspy_poe_port_power_watts").value.as_f64(), 4578.0 / 1000.0);
        assert_eq!(find(&metrics, "jaspy_poe_port_allocated_watts").value.as_f64(), 15400.0 / 1000.0);
        assert_eq!(find(&metrics, "jaspy_poe_port_max_drawn_watts").value.as_f64(), 5229.0 / 1000.0);
    }

    #[test]
    fn poe_metrics_standards_only_port_omits_watts_and_class() {
        use crate::collectors::poe::{InterfacePoe, PoeStatus};
        let mut poe = HashMap::new();
        poe.insert(501, InterfacePoe {
            admin_enabled: false,
            status: PoeStatus::Searching,
            class: None,
            power_mw: None,
            allocated_mw: None,
            max_drawn_mw: None,
            priority: None,
            ent_phy_index: None,
        });
        let metrics = poe_metrics(&poe_test_device(), &poe, &[], 1);
        let names: HashSet<&str> = metrics.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains("jaspy_poe_port_admin_enabled"));
        assert!(names.contains("jaspy_poe_port_status"));
        assert!(!names.contains("jaspy_poe_port_class"));
        assert!(!names.contains("jaspy_poe_port_power_watts"));
        assert!(!names.contains("jaspy_poe_port_allocated_watts"));
        assert!(!names.contains("jaspy_poe_port_max_drawn_watts"));
        assert_eq!(find(&metrics, "jaspy_poe_port_admin_enabled").value.as_i64(), 0);
        assert_eq!(find(&metrics, "jaspy_poe_port_status").value.as_i64(), 2); // searching
    }

    #[test]
    fn poe_metrics_unresolved_interface_id_leaves_name_empty() {
        use crate::collectors::poe::{InterfacePoe, PoeStatus};
        let mut poe = HashMap::new();
        poe.insert(999, InterfacePoe {
            admin_enabled: true,
            status: PoeStatus::Delivering,
            class: Some(3),
            power_mw: Some(1000),
            allocated_mw: None,
            max_drawn_mw: None,
            priority: None,
            ent_phy_index: None,
        });
        let metrics = poe_metrics(&poe_test_device(), &poe, &[], 1);
        let m = find(&metrics, "jaspy_poe_port_power_watts");
        assert_eq!(m.labels.get("name").map(String::as_str), Some(""));
        assert_eq!(m.labels.get("interface_id").map(String::as_str), Some("999"));
    }

    #[test]
    fn poe_metrics_budget_group_full() {
        use crate::collectors::poe::PoeBudget;
        let budget = vec![PoeBudget { group: 1, total_w: 124, consumed_w: 10, oper_on: true, threshold_pct: Some(80) }];
        let metrics = poe_metrics(&poe_test_device(), &HashMap::new(), &budget, 9);
        assert_eq!(
            find(&metrics, "jaspy_poe_budget_total_watts").as_text(),
            "jaspy_poe_budget_total_watts{fqdn=\"sw1.example.com\",hostname=\"sw1\",pse_group=\"1\"} 124 9"
        );
        assert_eq!(find(&metrics, "jaspy_poe_budget_consumed_watts").value.as_i64(), 10);
        assert_eq!(find(&metrics, "jaspy_poe_budget_oper_on").value.as_i64(), 1);
        assert_eq!(find(&metrics, "jaspy_poe_budget_threshold_percent").value.as_i64(), 80);
    }

    #[test]
    fn poe_metrics_budget_unset_threshold_omitted_and_oper_off() {
        use crate::collectors::poe::PoeBudget;
        let budget = vec![PoeBudget { group: 2, total_w: 60, consumed_w: 0, oper_on: false, threshold_pct: None }];
        let metrics = poe_metrics(&poe_test_device(), &HashMap::new(), &budget, 1);
        assert!(!metrics.iter().any(|m| m.name == "jaspy_poe_budget_threshold_percent"));
        assert_eq!(find(&metrics, "jaspy_poe_budget_oper_on").value.as_i64(), 0);
    }

    #[test]
    fn poe_metrics_empty_when_no_poe() {
        assert!(poe_metrics(&poe_test_device(), &HashMap::new(), &[], 1).is_empty());
    }

    // --- qos_metrics ---

    #[test]
    fn qos_metrics_class_with_policer_full_labels() {
        use crate::collectors::qos::{QosClass, QosDirection, QosPolice};
        let qos = vec![QosClass {
            interface_ifindex: 9,
            interface_name: Some("Fo1/0/1".to_string()),
            interface_id: Some(501),
            direction: QosDirection::Input,
            policymap: "httphttps".to_string(),
            classmap: "v6httphttps".to_string(),
            prepolicy_pkts: Some(4746055580),
            prepolicy_bytes: Some(13544055268755),
            postpolicy_bytes: Some(13544490413400),
            drop_pkts: Some(10926),
            drop_bytes: Some(16621056),
            police: Some(QosPolice {
                conform_pkts: Some(4745951234),
                conform_bytes: Some(13522486551429),
                exceed_pkts: Some(10926),
                exceed_bytes: Some(16621056),
                violate_pkts: None,
                violate_bytes: None,
            }),
        }];
        let metrics = qos_metrics(&poe_test_device(), &qos, 7);

        // Full, deterministically-sorted label set on a class counter.
        assert_eq!(
            find(&metrics, "jaspy_qos_class_prepolicy_packets_total").as_text(),
            "jaspy_qos_class_prepolicy_packets_total{classmap=\"v6httphttps\",direction=\"input\",fqdn=\"sw1.example.com\",hostname=\"sw1\",interface=\"Fo1/0/1\",interface_id=\"501\",policymap=\"httphttps\"} 4746055580 7"
        );
        // Live-verified policer values surface on the police_* series.
        assert_eq!(find(&metrics, "jaspy_qos_police_exceed_bytes_total").value.as_i64(), 16621056);
        assert_eq!(find(&metrics, "jaspy_qos_police_conform_bytes_total").value.as_i64(), 13522486551429);
        // None policer columns are omitted, not emitted as 0.
        let names: HashSet<&str> = metrics.iter().map(|m| m.name.as_str()).collect();
        assert!(!names.contains("jaspy_qos_police_violate_packets_total"));
        assert!(!names.contains("jaspy_qos_police_violate_bytes_total"));
    }

    #[test]
    fn qos_metrics_control_plane_has_no_interface_id_label() {
        use crate::collectors::qos::{QosClass, QosDirection};
        let qos = vec![QosClass {
            interface_ifindex: 0,
            interface_name: None,
            interface_id: None,
            direction: QosDirection::Input,
            policymap: "system-cpp-policy".to_string(),
            classmap: "class-default".to_string(),
            prepolicy_pkts: Some(5),
            prepolicy_bytes: None,
            postpolicy_bytes: None,
            drop_pkts: None,
            drop_bytes: None,
            police: None,
        }];
        let metrics = qos_metrics(&poe_test_device(), &qos, 3);
        // ifIndex 0 with no name renders the interface label as "control-plane"
        // and omits interface_id entirely.
        let m = find(&metrics, "jaspy_qos_class_prepolicy_packets_total");
        assert_eq!(m.labels.get("interface").map(|s| s.as_str()), Some("control-plane"));
        assert!(!m.labels.contains_key("interface_id"));
        // No policer, and absent class columns are omitted.
        let names: HashSet<&str> = metrics.iter().map(|m| m.name.as_str()).collect();
        assert!(!names.contains("jaspy_qos_class_drop_packets_total"));
        assert!(names.iter().all(|n| !n.starts_with("jaspy_qos_police_")));
    }

    #[test]
    fn qos_metrics_empty_when_no_qos() {
        assert!(qos_metrics(&poe_test_device(), &[], 1).is_empty());
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
    // Seeds the STP source probe order (see collectors::vendor).
    vendor: Vendor,
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
                vendor: vendor::vendor_hint(device.os_info.as_deref(), device.device_type.as_deref()),
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
pub(crate) fn fetch_table(snmp: &SnmpSource, host: &String, table: &str) -> Option<SNMPBotResponse> {
    // `host` is the inline `community@fqdn` / `community@vlan@fqdn` form.
    let spec = HostSpec::parse(host);
    match snmp.table(&spec, table) {
        Ok(parsed) => Some(parsed),
        Err(what) => {
            println!("[{}] snmp error for {} ({}), skipping", host, table, what);
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
        // Cisco stpx spells it "backUp"; HP-ICF-TC StpPortRole "backup".
        "backUp" | "backup" => 5,
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

fn get_entities(snmp: &SnmpSource, device: &EntityDevice, out: &mut Vec<LabeledMetric>, media_out: &mut HashMap<i32, String>) {
    let host = format!("{}@{}", device.community, device.fqdn);
    let phys = match fetch_table(snmp, &host, "ENTITY-MIB::entPhysicalTable") {
        Some(t) => t,
        None => return,
    };

    // Live media/form-factor overlay from the same table, resolved to db
    // interface ids via the name/description map (see entity_media for the
    // name-join rationale).
    for (name, value) in crate::collectors::entity_media::classify_media(&phys.entries) {
        if let Some((_, id)) = device.interfaces.get(&name) {
            media_out.insert(*id, value);
        }
    }

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
        snmp, device, &host, &identities, out,
        "ENTITY-SENSOR-MIB::entPhySensorValue",
        "ENTITY-SENSOR-MIB::entPhySensorScale",
        "ENTITY-SENSOR-MIB::entPhySensorPrecision",
        "ENTITY-SENSOR-MIB::entPhySensorType",
        "ENTITY-SENSOR-MIB::entPhySensorTable",
    );
    get_entities_by_physical_index(
        snmp, device, &host, &identities, out,
        "CISCO-ENTITY-SENSOR-MIB::entSensorValue",
        "CISCO-ENTITY-SENSOR-MIB::entSensorScale",
        "CISCO-ENTITY-SENSOR-MIB::entSensorPrecision",
        "CISCO-ENTITY-SENSOR-MIB::entSensorType",
        "CISCO-ENTITY-SENSOR-MIB::entSensorValueTable",
    );
}

fn get_entities_by_physical_index(
    snmp: &SnmpSource,
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
    let table = match fetch_table(snmp, host, table_field) {
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
// PoE polling (POWER-ETHERNET-MIB + CISCO-POWER-ETHERNET-EXT-MIB)
// ---------------------------------------------------------------------------

// Poll a device's PoE state: the vendor-neutral switch budget (pethMainPseTable)
// and per-port status/class (pethPsePortTable), overlaid with Cisco per-port
// watts (cpeExtPsePortTable) where available. Per-port rows are mapped to db
// interface ids via ENTITY-MIB entPhysicalName — the same join get_entities
// uses for media — which needs the Cisco entPhyIndex; standards-only devices
// keep only the switch-wide budget. Decode logic lives in collectors::poe.
fn get_poe(
    snmp: &SnmpSource,
    device: &EntityDevice,
    poe_out: &mut HashMap<i32, crate::collectors::poe::InterfacePoe>,
    budget_out: &mut Vec<crate::collectors::poe::PoeBudget>,
) {
    use crate::collectors::poe;
    let host = format!("{}@{}", device.community, device.fqdn);

    // Switch-wide budget first: an absent table is the "no PoE on this device"
    // signal (most non-PoE switches answer with an empty walk).
    if let Some(main) = fetch_table(snmp, &host, "POWER-ETHERNET-MIB::pethMainPseTable") {
        *budget_out = poe::decode_main_pse(&main);
    }

    let peth = match fetch_table(snmp, &host, "POWER-ETHERNET-MIB::pethPsePortTable") {
        Some(t) if !t.entries.is_empty() => t,
        // No per-port PoE table: keep whatever budget we captured and stop
        // before the extra ENTITY-MIB walk.
        _ => return,
    };
    let cpext = fetch_table(snmp, &host, "CISCO-POWER-ETHERNET-EXT-MIB::cpeExtPsePortTable");
    let ports = poe::decode_port_poe(&peth, cpext.as_ref());

    // entPhysicalIndex -> entPhysicalName, joined to db interface ids by name.
    let phys_names = entity_phys_names(snmp, &host);
    let iface_by_name: HashMap<String, i32> =
        device.interfaces.iter().map(|(key, (_, id))| (key.clone(), *id)).collect();
    *poe_out = poe::map_to_interfaces(&ports, &phys_names, &iface_by_name);
}

// entPhysicalIndex -> entPhysicalName from ENTITY-MIB. (get_entities walks the
// same table for sensors/media; PoE runs independently of the sensor flag, so
// it re-walks here — one extra walk per PoE device per cycle, negligible at the
// entitypoller's minutes-scale interval.)
fn entity_phys_names(snmp: &SnmpSource, host: &String) -> HashMap<i64, String> {
    let mut names: HashMap<i64, String> = HashMap::new();
    if let Some(phys) = fetch_table(snmp, host, "ENTITY-MIB::entPhysicalTable") {
        for entry in phys.entries.iter() {
            let id = match entry.index.get("ENTITY-MIB::entPhysicalIndex") {
                Some(v) => *v,
                None => continue,
            };
            if let Some(name) = obj_str(&entry.objects, "ENTITY-MIB::entPhysicalName") {
                names.insert(id, name);
            }
        }
    }
    names
}

// Build Prometheus samples from the decoded PoE state. Pure (no I/O), like the
// poe.rs decoders, so the label/units mapping is unit-tested from
// InterfacePoe/PoeBudget values; the run loop calls get_poe then appends these
// to the device's metric vec so render() emits them alongside sensors/STP. The
// Cisco per-port watts are milliwatts (scaled to watts here); the vendor-neutral
// pethMainPse budget is already in watts.
fn poe_metrics(
    device: &EntityDevice,
    poe: &HashMap<i32, crate::collectors::poe::InterfacePoe>,
    budget: &[crate::collectors::poe::PoeBudget],
    timestamp: u64,
) -> Vec<LabeledMetric> {
    let mut out: Vec<LabeledMetric> = Vec::new();

    // db interface id -> interface name. device.interfaces is keyed by both name
    // and description, each pointing at the same (name, id); invert it once.
    let mut name_by_id: HashMap<i32, String> = HashMap::new();
    for (_, (name, id)) in device.interfaces.iter() {
        name_by_id.entry(*id).or_insert_with(|| name.clone());
    }

    let base_labels = |extra: &[(&str, String)]| -> HashMap<String, String> {
        let mut labels: HashMap<String, String> = HashMap::new();
        labels.insert("fqdn".to_string(), device.fqdn.clone());
        labels.insert("hostname".to_string(), device.hostname.clone());
        for (key, value) in extra {
            labels.insert(key.to_string(), value.clone());
        }
        labels
    };
    let mut push = |out: &mut Vec<LabeledMetric>, name: &str, value: MetricValue, labels: &HashMap<String, String>| {
        out.push(LabeledMetric::new(&name.to_string(), value, labels, timestamp));
    };

    // Per-port samples. Watts are Cisco-extension only (None on standards-only
    // devices), so those rows are skipped when absent rather than reported as 0.
    for (iface_id, port) in poe.iter() {
        let name = name_by_id.get(iface_id).cloned().unwrap_or_default();
        let labels = base_labels(&[("name", name), ("interface_id", iface_id.to_string())]);
        push(&mut out, "jaspy_poe_port_admin_enabled", MetricValue::Int64(if port.admin_enabled { 1 } else { 0 }), &labels);
        push(&mut out, "jaspy_poe_port_status", MetricValue::Int64(port.status.as_numeric()), &labels);
        if let Some(class) = port.class {
            push(&mut out, "jaspy_poe_port_class", MetricValue::Int64(class), &labels);
        }
        if let Some(mw) = port.power_mw {
            push(&mut out, "jaspy_poe_port_power_watts", MetricValue::Float64(mw as f64 / 1000.0), &labels);
        }
        if let Some(mw) = port.allocated_mw {
            push(&mut out, "jaspy_poe_port_allocated_watts", MetricValue::Float64(mw as f64 / 1000.0), &labels);
        }
        if let Some(mw) = port.max_drawn_mw {
            push(&mut out, "jaspy_poe_port_max_drawn_watts", MetricValue::Float64(mw as f64 / 1000.0), &labels);
        }
    }

    // Switch-wide budget, one set of samples per PSE group.
    for group in budget.iter() {
        let labels = base_labels(&[("pse_group", group.group.to_string())]);
        push(&mut out, "jaspy_poe_budget_total_watts", MetricValue::Int64(group.total_w), &labels);
        push(&mut out, "jaspy_poe_budget_consumed_watts", MetricValue::Int64(group.consumed_w), &labels);
        push(&mut out, "jaspy_poe_budget_oper_on", MetricValue::Int64(if group.oper_on { 1 } else { 0 }), &labels);
        if let Some(pct) = group.threshold_pct {
            push(&mut out, "jaspy_poe_budget_threshold_percent", MetricValue::Int64(pct), &labels);
        }
    }

    out
}

// ---------------------------------------------------------------------------
// QoS polling (CISCO-CLASS-BASED-QOS-MIB): Cisco policy-map counters, a
// core-router feature. Gated so switches without service-policies are never
// walked: a confirmed empty cbQosServicePolicyTable is remembered in the
// QosProbeCache for QOS_NEGATIVE_TTL, and non-Cisco devices are skipped by the
// discovery vendor hint. Decode + cache logic live in collectors::qos.
// ---------------------------------------------------------------------------

// Poll a device's QoS state. Returns leaving `qos_out` empty (so the overlay is
// forgotten) whenever the device has no policy-maps, is non-Cisco, is skipped by
// the negative cache, or an SNMP walk errors. The cache is only updated on an
// authoritative answer: an empty service-policy walk records a negative; a
// non-empty one clears it. Transient SNMP errors are never cached.
fn get_qos(
    snmp: &SnmpSource,
    device: &EntityDevice,
    cache: &crate::collectors::qos::QosProbeCache,
    now: std::time::Instant,
    qos_out: &mut Vec<crate::collectors::qos::QosClass>,
) {
    use crate::collectors::qos;

    // CBQoS is Cisco-only; skip the probe entirely for other vendors.
    if device.vendor != Vendor::Cisco {
        return;
    }
    if cache.should_skip(&device.fqdn, now) {
        return;
    }
    let host = format!("{}@{}", device.community, device.fqdn);

    // The presence gate: an empty walk is the authoritative "no policy-maps"
    // signal; an SNMP error (None) is transient and must not be cached.
    let service_policy = match fetch_table(snmp, &host, "CISCO-CLASS-BASED-QOS-MIB::cbQosServicePolicyTable") {
        Some(t) => t,
        None => return,
    };
    if service_policy.entries.is_empty() {
        cache.record_absent(&device.fqdn, now);
        return;
    }
    cache.note_present(&device.fqdn);

    // Pull the rest of the join. Any errored walk aborts this cycle (the overlay
    // is left empty rather than storing a half-joined view).
    let objects = match fetch_table(snmp, &host, "CISCO-CLASS-BASED-QOS-MIB::cbQosObjectsTable") {
        Some(t) => t,
        None => return,
    };
    let policymap_cfg = match fetch_table(snmp, &host, "CISCO-CLASS-BASED-QOS-MIB::cbQosPolicyMapCfgTable") {
        Some(t) => t,
        None => return,
    };
    let cm_cfg = match fetch_table(snmp, &host, "CISCO-CLASS-BASED-QOS-MIB::cbQosCMCfgTable") {
        Some(t) => t,
        None => return,
    };
    let cm_stats = match fetch_table(snmp, &host, "CISCO-CLASS-BASED-QOS-MIB::cbQosCMStatsTable") {
        Some(t) => t,
        None => return,
    };
    let police_stats = match fetch_table(snmp, &host, "CISCO-CLASS-BASED-QOS-MIB::cbQosPoliceStatsTable") {
        Some(t) => t,
        None => return,
    };

    let if_names = get_if_names(snmp, &host);
    let mut classes = qos::decode_qos(
        &service_policy, &objects, &policymap_cfg, &cm_cfg, &cm_stats, &police_stats, &if_names,
    );

    // Resolve the db interface id from the ifName (device.interfaces is keyed by
    // both name and description); leaves None for control-plane / unmapped.
    let iface_by_name: HashMap<String, i32> =
        device.interfaces.iter().map(|(key, (_, id))| (key.clone(), *id)).collect();
    for class in classes.iter_mut() {
        class.interface_id = class
            .interface_name
            .as_ref()
            .and_then(|n| iface_by_name.get(n))
            .map(|id| *id as i64);
    }
    *qos_out = classes;
}

// ifIndex -> ifName from IF-MIB::ifXTable, for labeling QoS rows with the
// interface the service-policy is applied to.
fn get_if_names(snmp: &SnmpSource, host: &String) -> HashMap<i64, String> {
    let mut names: HashMap<i64, String> = HashMap::new();
    if let Some(ifx) = fetch_table(snmp, host, "IF-MIB::ifXTable") {
        for entry in ifx.entries.iter() {
            let idx = match entry.index.get("IF-MIB::ifIndex") {
                Some(v) => *v,
                None => continue,
            };
            if let Some(name) = obj_str(&entry.objects, "IF-MIB::ifName") {
                names.insert(idx, name);
            }
        }
    }
    names
}

// Convert stored QosClass rows to the API DTO (used by device_entity()). Free
// function so the store method can call it without a device handle — everything
// it needs (name, id, counters) is already on QosClass.
fn qos_to_api(classes: &[crate::collectors::qos::QosClass]) -> Vec<crate::models::json::ApiQosClass> {
    use crate::models::json::{ApiQosClass, ApiQosPolice};
    classes.iter().map(|c| ApiQosClass {
        interface: c.interface_name.clone(),
        interface_id: c.interface_id,
        direction: c.direction.as_str().to_string(),
        policy_map: c.policymap.clone(),
        class_map: c.classmap.clone(),
        prepolicy_pkts: c.prepolicy_pkts,
        prepolicy_bytes: c.prepolicy_bytes,
        postpolicy_bytes: c.postpolicy_bytes,
        drop_pkts: c.drop_pkts,
        drop_bytes: c.drop_bytes,
        police: c.police.as_ref().map(|p| ApiQosPolice {
            conform_pkts: p.conform_pkts,
            conform_bytes: p.conform_bytes,
            exceed_pkts: p.exceed_pkts,
            exceed_bytes: p.exceed_bytes,
            violate_pkts: p.violate_pkts,
            violate_bytes: p.violate_bytes,
        }),
    }).collect()
}

// Build Prometheus samples from the decoded QoS state. Pure (no I/O), like
// poe_metrics, so the label/name mapping is unit-tested from QosClass values.
// All counters are monotonic (Counter64), emitted as _total; a class/policer
// column the device omits is skipped rather than reported as 0.
fn qos_metrics(
    device: &EntityDevice,
    qos: &[crate::collectors::qos::QosClass],
    timestamp: u64,
) -> Vec<LabeledMetric> {
    let mut out: Vec<LabeledMetric> = Vec::new();
    for class in qos.iter() {
        // Interface label: ifName when resolved, "control-plane" for a
        // control-plane policy (ifIndex 0), else the raw ifIndex.
        let interface = class.interface_name.clone().unwrap_or_else(|| {
            if class.interface_ifindex == 0 {
                "control-plane".to_string()
            } else {
                class.interface_ifindex.to_string()
            }
        });
        let mut labels: HashMap<String, String> = HashMap::new();
        labels.insert("fqdn".to_string(), device.fqdn.clone());
        labels.insert("hostname".to_string(), device.hostname.clone());
        labels.insert("interface".to_string(), interface);
        labels.insert("direction".to_string(), class.direction.as_str().to_string());
        labels.insert("policymap".to_string(), class.policymap.clone());
        labels.insert("classmap".to_string(), class.classmap.clone());
        if let Some(id) = class.interface_id {
            labels.insert("interface_id".to_string(), id.to_string());
        }

        let mut push = |name: &str, value: Option<u64>| {
            if let Some(v) = value {
                out.push(LabeledMetric::new(&name.to_string(), MetricValue::Uint64(v), &labels, timestamp));
            }
        };
        push("jaspy_qos_class_prepolicy_packets_total", class.prepolicy_pkts);
        push("jaspy_qos_class_prepolicy_bytes_total", class.prepolicy_bytes);
        push("jaspy_qos_class_postpolicy_bytes_total", class.postpolicy_bytes);
        push("jaspy_qos_class_drop_packets_total", class.drop_pkts);
        push("jaspy_qos_class_drop_bytes_total", class.drop_bytes);
        if let Some(p) = class.police.as_ref() {
            push("jaspy_qos_police_conform_packets_total", p.conform_pkts);
            push("jaspy_qos_police_conform_bytes_total", p.conform_bytes);
            push("jaspy_qos_police_exceed_packets_total", p.exceed_pkts);
            push("jaspy_qos_police_exceed_bytes_total", p.exceed_bytes);
            push("jaspy_qos_police_violate_packets_total", p.violate_pkts);
            push("jaspy_qos_police_violate_bytes_total", p.violate_bytes);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// STP polling, one vendor::Source per incompatible MIB family:
//   - CiscoStpxSource: CISCO-STP-EXTENSIONS-MIB roles + per-VLAN BRIDGE-MIB
//   - HpRpvstSource:   HP-ICF-RPVST-MIB (ProCurve RPVST+)
// The probe order and per-device winner cache live in collectors::vendor.
// ---------------------------------------------------------------------------

struct StpCtx<'a> {
    snmp: &'a SnmpSource,
    device: &'a EntityDevice,
    host: &'a String,
}

fn get_stp(snmp: &SnmpSource, device: &EntityDevice, cache: &vendor::SourceCache, out: &mut Vec<LabeledMetric>) {
    let host = format!("{}@{}", device.community, device.fqdn);
    let ctx = StpCtx { snmp: snmp, device: device, host: &host };
    let sources: [&dyn vendor::Source<StpCtx, Output = Vec<LabeledMetric>>; 2] = [&CiscoStpxSource, &HpRpvstSource];
    if let Some(metrics) = vendor::collect_first(cache, &device.fqdn, device.vendor, &sources, &ctx) {
        out.extend(metrics);
    }
    // All sources None: the device exposes no STP data (or is unreachable);
    // nothing to add, and the next cycle re-probes from the hint order.
}

#[derive(Clone)]
struct StpPortInfo {
    role: String,
    interface_id: String,   // real ifIndex (from dot1dBasePortIfIndex), "0" if unknown
    interface_name: String, // ifDescr of that ifIndex, "UNKNOWN" if unknown
}

struct CiscoStpxSource;

impl<'a> vendor::Source<StpCtx<'a>> for CiscoStpxSource {
    type Output = Vec<LabeledMetric>;

    fn name(&self) -> &'static str {
        "cisco-stpx"
    }

    fn vendor(&self) -> Vendor {
        Vendor::Cisco
    }

    fn collect(&self, ctx: &StpCtx) -> Option<Vec<LabeledMetric>> {
        cisco_stpx_collect(ctx.snmp, ctx.device, ctx.host)
    }
}

fn cisco_stpx_collect(snmp: &SnmpSource, device: &EntityDevice, host: &String) -> Option<Vec<LabeledMetric>> {
    // The role table is the applicability probe: absent or empty means this
    // device doesn't speak CISCO-STP-EXTENSIONS-MIB.
    let role_table = match fetch_table(snmp, host, "CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleTable") {
        Some(t) if !t.entries.is_empty() => t,
        _ => return None,
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

    let mut out: Vec<LabeledMetric> = Vec::new();

    // ifTable (base community) for real ifIndex -> ifDescr names. The role
    // table already answered, so this device IS the Cisco source's — report
    // it as such (with whatever collected) rather than falling through to
    // the other sources on a transient failure.
    let iftable = match fetch_table(snmp, host, "IF-MIB::ifTable") {
        Some(t) => t,
        None => return Some(out),
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
        // A per-VLAN fetch failure skips only this VLAN: aborting the whole
        // device on one bad community@vlan context would drop a random subset
        // of the remaining VLANs (HashMap order) and make the tree flicker.
        let base_table = match fetch_table(snmp, &per_vlan_host, "BRIDGE-MIB::dot1dBasePortTable") {
            Some(t) => t,
            None => continue,
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
        let stp_table = match fetch_table(snmp, &per_vlan_host, "BRIDGE-MIB::dot1dStpPortTable") {
            Some(t) => t,
            None => continue,
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

            push_stp_metric(&mut out, device, port, *vlan, bridge_port, "port_designated_cost", designated_cost, timestamp);
            push_stp_metric(&mut out, device, port, *vlan, bridge_port, "port_path_cost", path_cost, timestamp);
            push_stp_metric(&mut out, device, port, *vlan, bridge_port, "port_priority", priority, timestamp);
            push_stp_metric(&mut out, device, port, *vlan, bridge_port, "port_forward_transitions", forward_transitions, timestamp);
            push_stp_metric(&mut out, device, port, *vlan, bridge_port, "port_enabled", stp_port_enable_numeric(&enable), timestamp);
            push_stp_metric(&mut out, device, port, *vlan, bridge_port, "port_role", rstp_port_role_numeric(&port.role), timestamp);
            push_stp_metric(&mut out, device, port, *vlan, bridge_port, "port_state", stp_port_state_numeric(&state), timestamp);
        }

        // Bridge-level scalars for this vlan (BRIDGE-MIB dot1dStp group): the
        // reported root identity for cross-checking the computed tree, plus
        // topology-change churn. One batched objects GET per vlan per cycle.
        get_stp_bridge(snmp, device, &per_vlan_host, *vlan, timestamp, &mut out);
    }

    Some(out)
}

// ---------------------------------------------------------------------------
// HP RPVST+ source (HP-ICF-RPVST-MIB). ProCurve switches expose per-VLAN
// spanning tree only here: neither classic BRIDGE-MIB dot1dStp nor the Cisco
// stpx tables answer (verified on 2530-8G, YA.15.16). The port-vlan data is
// fetched through jaspy-specific single-column table views (see
// snmpbot/mibs/HP-ICF-RPVST-MIB.json) because snmpbot's lockstep multi-column
// walk misaligns on this sparse table. Port index == ifIndex on ProCurve.
// ---------------------------------------------------------------------------

struct HpRpvstSource;

impl<'a> vendor::Source<StpCtx<'a>> for HpRpvstSource {
    type Output = Vec<LabeledMetric>;

    fn name(&self) -> &'static str {
        "hp-rpvst"
    }

    fn vendor(&self) -> Vendor {
        Vendor::HpProcurve
    }

    fn collect(&self, ctx: &StpCtx) -> Option<Vec<LabeledMetric>> {
        hp_rpvst_collect(ctx.snmp, ctx.device, ctx.host)
    }
}

fn hp_rpvst_collect(snmp: &SnmpSource, device: &EntityDevice, host: &String) -> Option<Vec<LabeledMetric>> {
    let roles = match fetch_table(snmp, host, "HP-ICF-RPVST-MIB::jaspyRpvstPortVlanRoleTable") {
        Some(t) if !t.entries.is_empty() => t,
        _ => return None, // no RPVST — not this source's device
    };
    let states = fetch_table(snmp, host, "HP-ICF-RPVST-MIB::jaspyRpvstPortVlanStateTable");
    let costs = fetch_table(snmp, host, "HP-ICF-RPVST-MIB::jaspyRpvstPortVlanCostTable");
    let vlans = fetch_table(snmp, host, "HP-ICF-RPVST-MIB::hpicfRpvstVlanTable");

    // Real ifIndex -> ifDescr names (port index == ifIndex on ProCurve).
    let mut interfaces: HashMap<i64, String> = HashMap::new();
    if let Some(iftable) = fetch_table(snmp, host, "IF-MIB::ifTable") {
        for entry in iftable.entries.iter() {
            if let (Some(ifidx), Some(descr)) = (entry.index.get("IF-MIB::ifIndex"), obj_str(&entry.objects, "IF-MIB::ifDescr")) {
                interfaces.insert(*ifidx, descr);
            }
        }
    }

    let timestamp = tools::get_time_msecs();
    Some(rpvst_metrics(device, &interfaces, &roles, states.as_ref(), costs.as_ref(), vlans.as_ref(), timestamp))
}

// (vlan, port) index of an RPVST port-vlan row.
fn rpvst_row_key(entry: &crate::collectors::poller::SNMPBotResultEntry) -> Option<(i64, i64)> {
    let vlan = entry.index.get("HP-ICF-RPVST-MIB::hpicfRpvstVlanId")?;
    let port = entry.index.get("HP-ICF-RPVST-MIB::hpicfRpvstPortIndex")?;
    Some((*vlan, *port))
}

fn rpvst_metrics(
    device: &EntityDevice,
    interfaces: &HashMap<i64, String>,
    roles: &SNMPBotResponse,
    states: Option<&SNMPBotResponse>,
    costs: Option<&SNMPBotResponse>,
    vlans: Option<&SNMPBotResponse>,
    timestamp: u64,
) -> Vec<LabeledMetric> {
    let mut out: Vec<LabeledMetric> = Vec::new();

    let mut state_by_key: HashMap<(i64, i64), String> = HashMap::new();
    for entry in states.map(|t| t.entries.iter()).into_iter().flatten() {
        if let (Some(key), Some(state)) = (rpvst_row_key(entry), obj_str(&entry.objects, "HP-ICF-RPVST-MIB::hpicfRpvstPortVlanState")) {
            state_by_key.insert(key, state);
        }
    }
    let mut cost_by_key: HashMap<(i64, i64), i64> = HashMap::new();
    for entry in costs.map(|t| t.entries.iter()).into_iter().flatten() {
        if let (Some(key), Some(cost)) = (rpvst_row_key(entry), obj_i64(&entry.objects, "HP-ICF-RPVST-MIB::hpicfRpvstPortVlanPathCost")) {
            cost_by_key.insert(key, cost);
        }
    }

    for entry in roles.entries.iter() {
        let (vlan, port) = match rpvst_row_key(entry) {
            Some(key) => key,
            None => continue,
        };
        // Out-of-enum raw values (observed: 0) arrive as numbers, not enum
        // strings — skip those rows.
        let role = match obj_str(&entry.objects, "HP-ICF-RPVST-MIB::hpicfRpvstPortVlanRole") {
            Some(role) => role,
            None => continue,
        };
        let info = StpPortInfo {
            role: role,
            interface_id: port.to_string(),
            interface_name: interfaces.get(&port).cloned().unwrap_or_else(|| "UNKNOWN".to_string()),
        };
        let state = state_by_key.get(&(vlan, port)).cloned().unwrap_or_default();
        let cost = cost_by_key.get(&(vlan, port)).cloned().unwrap_or(0);
        push_stp_metric(&mut out, device, &info, vlan, port, "port_path_cost", cost, timestamp);
        push_stp_metric(&mut out, device, &info, vlan, port, "port_role", rstp_port_role_numeric(&info.role), timestamp);
        push_stp_metric(&mut out, device, &info, vlan, port, "port_state", stp_port_state_numeric(&state), timestamp);
    }

    for entry in vlans.map(|t| t.entries.iter()).into_iter().flatten() {
        let vlan = match entry.index.get("HP-ICF-RPVST-MIB::hpicfRpvstVlanId") {
            Some(v) => *v,
            None => continue,
        };
        if let Some(priority) = obj_i64(&entry.objects, "HP-ICF-RPVST-MIB::hpicfRpvstVlanRootPriority") {
            let mac = obj_str(&entry.objects, "HP-ICF-RPVST-MIB::hpicfRpvstVlanRootMacAddress")
                .map(|m| crate::utilities::stp::normalize_mac(&m))
                .unwrap_or_default();
            push_stp_bridge_metric(&mut out, device, vlan, "root_priority", priority, &[("root_mac", &mac)], timestamp);
        }
        if let Some(cost) = obj_i64(&entry.objects, "HP-ICF-RPVST-MIB::hpicfRpvstVlanRootPathCost") {
            push_stp_bridge_metric(&mut out, device, vlan, "root_cost", cost, &[], timestamp);
        }
        if let Some(port) = obj_i64(&entry.objects, "HP-ICF-RPVST-MIB::hpicfRpvstVlanRootPort") {
            push_stp_bridge_metric(&mut out, device, vlan, "root_port", port, &[], timestamp);
        }
        if let Some(changes) = obj_i64(&entry.objects, "HP-ICF-RPVST-MIB::hpicfVlanTopoChangeCount") {
            push_stp_bridge_metric(&mut out, device, vlan, "topology_changes", changes, &[], timestamp);
        }
        if let Some(secs) = entry.objects.get("HP-ICF-RPVST-MIB::hpicfRpvstVlanTimeSinceLastTopoChange").and_then(crate::utilities::stp::timeticks_secs) {
            push_stp_bridge_metric(&mut out, device, vlan, "time_since_topology_change", secs, &[], timestamp);
        }
    }

    out
}

fn value_i64(value: &SNMPBotResultEntryObjectValue) -> Option<i64> {
    match value {
        SNMPBotResultEntryObjectValue::Uint64(v) => Some(*v as i64),
        SNMPBotResultEntryObjectValue::Float64(v) => Some(*v as i64),
        _ => None,
    }
}

// jaspyStpBridgeTable is a jaspy-specific view over the five BRIDGE-MIB
// dot1dStp scalars (snmpbot/mibs/BRIDGE-MIB.json): one table walk fetches
// them all, where five single-object GETs would each carry a full transient-
// host MIB probe. Real snmpbot can't batch them via `/objects/?object=`
// either — that form only spans MIBs its probe detected, and BRIDGE-MIB
// probing fails on gear that answers dot1dStp GETs fine (verified on a live
// C2960CX).
fn get_stp_bridge(snmp: &SnmpSource, device: &EntityDevice, per_vlan_host: &String, vlan: i64, timestamp: u64, out: &mut Vec<LabeledMetric>) {
    let table = match fetch_table(snmp, per_vlan_host, "BRIDGE-MIB::jaspyStpBridgeTable") {
        Some(t) => t,
        None => return,
    };
    // Scalars: a single row (index .0).
    if let Some(entry) = table.entries.first() {
        out.extend(bridge_scalar_metrics(device, vlan, &entry.objects, timestamp));
    }
}

fn bridge_scalar_metrics(device: &EntityDevice, vlan: i64, values: &HashMap<String, SNMPBotResultEntryObjectValue>, timestamp: u64) -> Vec<LabeledMetric> {
    use crate::utilities::stp::{parse_bridge_id, timeticks_secs};

    let mut out: Vec<LabeledMetric> = Vec::new();
    if let Some(SNMPBotResultEntryObjectValue::Str(raw)) = values.get("BRIDGE-MIB::dot1dStpDesignatedRoot") {
        if let Some((priority, mac)) = parse_bridge_id(raw) {
            push_stp_bridge_metric(&mut out, device, vlan, "root_priority", priority, &[("root_mac", &mac)], timestamp);
        } else {
            println!("[{}] unparseable dot1dStpDesignatedRoot: {:.40}", device.fqdn, raw);
        }
    }
    if let Some(cost) = values.get("BRIDGE-MIB::dot1dStpRootCost").and_then(value_i64) {
        push_stp_bridge_metric(&mut out, device, vlan, "root_cost", cost, &[], timestamp);
    }
    if let Some(port) = values.get("BRIDGE-MIB::dot1dStpRootPort").and_then(value_i64) {
        push_stp_bridge_metric(&mut out, device, vlan, "root_port", port, &[], timestamp);
    }
    if let Some(changes) = values.get("BRIDGE-MIB::dot1dStpTopChanges").and_then(value_i64) {
        push_stp_bridge_metric(&mut out, device, vlan, "topology_changes", changes, &[], timestamp);
    }
    if let Some(secs) = values.get("BRIDGE-MIB::dot1dStpTimeSinceTopologyChange").and_then(timeticks_secs) {
        push_stp_bridge_metric(&mut out, device, vlan, "time_since_topology_change", secs, &[], timestamp);
    }
    out
}

fn push_stp_bridge_metric(out: &mut Vec<LabeledMetric>, device: &EntityDevice, vlan: i64, key: &str, value: i64, extra_labels: &[(&str, &str)], timestamp: u64) {
    let mut labels: HashMap<String, String> = HashMap::new();
    labels.insert("hostname".to_string(), device.hostname.clone());
    labels.insert("fqdn".to_string(), device.fqdn.clone());
    labels.insert("vlan".to_string(), vlan.to_string());
    for (label_key, label_value) in extra_labels {
        labels.insert(label_key.to_string(), label_value.to_string());
    }
    out.push(LabeledMetric::new(
        &format!("jaspy_stp_bridge_{}", key),
        MetricValue::Int64(value),
        &labels,
        timestamp,
    ));
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

// Cap on simultaneous per-device poll threads (each holds a blocking HTTP
// connection to snmpbot for its device's whole sensor+STP sequence).
const MAX_POLL_WORKERS: usize = 16;

pub(crate) fn interruptible_sleep(msecs: u64, running: &Arc<atomic::AtomicBool>) {
    let mut slept = 0;
    while slept < msecs && running.load(atomic::Ordering::Relaxed) {
        let chunk = std::cmp::min(250, msecs - slept);
        thread::sleep(time::Duration::from_millis(chunk));
        slept += chunk;
    }
}

pub fn run(snmp: Arc<SnmpSource>, interval_msecs: u64, disable_sensors: bool, disable_stp: bool, disable_qos: bool, store: Arc<Mutex<EntityMetricsStore>>, control: Arc<super::PollingControl>, running: Arc<atomic::AtomicBool>) {
    println!("[entitypoller] starting in-process collector (interval_msecs={}, sensors={}, stp={}, qos={})",
        interval_msecs, !disable_sensors, !disable_stp, !disable_qos);
    let pool = db::connect();
    let no_jitter = std::env::var("JASPY_POLLER_NO_JITTER").map(|v| v == "1" || v == "true").unwrap_or(false);
    let stp_sources = Arc::new(vendor::SourceCache::new());
    // Negative-probe cache: remembers which devices have no policy-maps so we
    // don't re-walk them every cycle (survives across cycles, unlike the
    // per-cycle device list). Cloned into each worker below.
    let qos_cache = Arc::new(crate::collectors::qos::QosProbeCache::new());

    while running.load(atomic::Ordering::Relaxed) {
        // Paused via the master switch: skip the whole poll cycle so no SNMP is
        // issued. The last sensor/STP/PoE/QoS samples stay in the store but the
        // /dev/metrics exporter is gated, so Prometheus doesn't scrape them.
        if !control.enabled() {
            interruptible_sleep(1000, &running);
            continue;
        }
        let cycle_start = tools::get_time_msecs();
        let devices = load_devices(&pool);

        // Drop metrics + cached STP sources for devices no longer monitored.
        let keep: HashSet<String> = devices.iter().map(|d| d.fqdn.clone()).collect();
        if let Ok(mut store) = store.lock() {
            store.retain(&keep);
        }
        stp_sources.retain(&keep);
        qos_cache.retain(&keep);

        // Bounded per-device fan-out, joined at a barrier (the Go original
        // spawned one goroutine per device via runOnce+WaitGroup).
        let jitter = if no_jitter { 0 } else { interval_msecs / 2 };
        crate::collectors::pool::run_bounded(devices, MAX_POLL_WORKERS, jitter, |device| {
            let mut metrics: Vec<LabeledMetric> = Vec::new();
            let mut media: HashMap<i32, String> = HashMap::new();
            let mut poe: HashMap<i32, crate::collectors::poe::InterfacePoe> = HashMap::new();
            let mut poe_budget: Vec<crate::collectors::poe::PoeBudget> = Vec::new();
            let mut qos: Vec<crate::collectors::qos::QosClass> = Vec::new();
            if !disable_sensors {
                get_entities(&snmp, &device, &mut metrics, &mut media);
            }
            if !disable_stp {
                get_stp(&snmp, &device, &stp_sources, &mut metrics);
            }
            get_poe(&snmp, &device, &mut poe, &mut poe_budget);
            // Emit PoE as Prometheus samples too (the store also keeps the
            // structured poe/poe_budget for the /api/v1 overlay). Appended to
            // the same vec so render() picks them up; decode_entity ignores the
            // jaspy_poe_* names.
            metrics.extend(poe_metrics(&device, &poe, &poe_budget, tools::get_time_msecs()));
            if !disable_qos {
                get_qos(&snmp, &device, &qos_cache, std::time::Instant::now(), &mut qos);
                // Same pattern as PoE: emit jaspy_qos_* samples and keep the
                // structured rows for the /api/v1 device_entity overlay.
                metrics.extend(qos_metrics(&device, &qos, tools::get_time_msecs()));
            }
            if let Ok(mut store) = store.lock() {
                store.replace_device(device.fqdn.clone(), metrics);
                store.set_media(device.fqdn.clone(), media);
                store.set_poe(device.fqdn.clone(), poe);
                store.set_poe_budget(device.fqdn.clone(), poe_budget);
                store.set_qos(device.fqdn.clone(), qos);
            }
        });

        let elapsed = tools::get_time_msecs() - cycle_start;
        if elapsed < interval_msecs {
            interruptible_sleep(interval_msecs - elapsed, &running);
        }
    }
    println!("[entitypoller] collector stopped");
}
