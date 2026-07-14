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
fn fetch_table(snmpbot_url: &String, host: &String, table: &str) -> Option<SNMPBotResponse> {
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

fn obj_i64(objects: &HashMap<String, SNMPBotResultEntryObjectValue>, key: &str) -> Option<i64> {
    match objects.get(key) {
        Some(SNMPBotResultEntryObjectValue::Uint64(v)) => Some(*v as i64),
        Some(SNMPBotResultEntryObjectValue::Float64(v)) => Some(*v as i64),
        _ => None,
    }
}

fn obj_str(objects: &HashMap<String, SNMPBotResultEntryObjectValue>, key: &str) -> Option<String> {
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

fn interruptible_sleep(msecs: u64, running: &Arc<atomic::AtomicBool>) {
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
