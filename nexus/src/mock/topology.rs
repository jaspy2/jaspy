// The fake network served by `jaspy-nexus mock`: a small campus (core, two
// distribution switches, three access switches, a WLC and a firewall) with
// LLDP adjacency, sensors and per-VLAN STP. Every value is a pure function of
// elapsed time since startup — counters grow, sensors drift, one uplink flaps
// — so there is no mutation thread and no locking.
//
// The generated tables mirror the snmpbot response shapes in tests/fixtures/
// exactly; they serialize through the same SNMPBotResponse structs the
// collectors deserialize, so shape compatibility holds by construction.
use crate::collectors::poller::{SNMPBotResponse, SNMPBotResultEntry};
use serde_json::json;

pub const DOMAIN: &str = "mock.jaspy";
pub const COMMUNITY: &str = "mock";
pub const ROOT_DEVICE: &str = "core1.mock.jaspy";
// Flapping uplink: up for FLAP_HALF_PERIOD_SECS, down for the same, repeat.
pub const FLAP_HALF_PERIOD_SECS: u64 = 60;

#[derive(Clone, Copy, PartialEq)]
pub enum SensorStyle {
    Standard, // ENTITY-SENSOR-MIB::entPhySensorTable
    Cisco,    // CISCO-ENTITY-SENSOR-MIB::entSensorValueTable
    None,     // sensor tables answer 404
}

// Which VLAN membership MIBs the device answers (vlanpoller tries Cisco first,
// then falls back to Q-BRIDGE).
#[derive(Clone, Copy, PartialEq)]
pub enum VlanStyle {
    Cisco,   // CISCO-VTP-MIB + CISCO-VLAN-MEMBERSHIP-MIB
    QBridge, // Q-BRIDGE-MIB + un-vlan-indexed BRIDGE-MIB::dot1dBasePortTable
    None,    // VLAN tables answer 404
}

pub struct MockInterface {
    pub ifindex: i64,
    pub name: &'static str,  // short form, becomes ifName ("Te1/0/1")
    pub descr: &'static str, // long form, becomes ifDescr
    pub alias: &'static str,
    pub speed_mbps: u64,
    // (bare peer device name, peer interface name); declared on one side,
    // emitted into LLDP tables symmetrically.
    pub peer: Option<(&'static str, &'static str)>,
    pub flaps: bool,
    // Static state for access ports without a peer (a realistic mix).
    pub up: bool,
}

pub struct MockDevice {
    pub name: &'static str,
    pub model: &'static str,
    pub sys_descr: &'static str,
    pub sw_rev: &'static str,
    pub sensor_style: SensorStyle,
    pub stp_vlans: &'static [i64],
    pub vlan_style: VlanStyle,
    // VLANs that exist on the device (vtpVlanTable / dot1qVlanCurrentTable).
    // Peered interfaces are trunks: native TRUNK_NATIVE_VLAN, tagged = the
    // rest. Unpeered ports are access ports on access_vlan(ifindex).
    pub vlans: &'static [i64],
    pub interfaces: Vec<MockInterface>,
}

impl MockDevice {
    // Used by the mock unit tests; the runtime paths key off bare names.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn fqdn(&self) -> String {
        format!("{}.{}", self.name, DOMAIN)
    }
}

pub struct Topology {
    pub devices: Vec<MockDevice>,
    pub started: f64,
}

fn uplink(ifindex: i64, name: &'static str, descr: &'static str, alias: &'static str, speed: u64, peer: (&'static str, &'static str)) -> MockInterface {
    MockInterface { ifindex, name, descr, alias, speed_mbps: speed, peer: Some(peer), flaps: false, up: true }
}

fn access_port(ifindex: i64, name: &'static str, descr: &'static str, up: bool) -> MockInterface {
    MockInterface { ifindex, name, descr, alias: "", speed_mbps: 1000, peer: None, flaps: false, up }
}

pub fn build() -> Topology {
    let devices = vec![
        MockDevice {
            name: "core1",
            model: "C9606R",
            sys_descr: "Cisco IOS-XE Software (mock), Catalyst 9600 Switch",
            sw_rev: "17.9.4a",
            sensor_style: SensorStyle::Cisco,
            stp_vlans: &[10, 20],
            vlan_style: VlanStyle::Cisco,
            vlans: &[1, 10, 20],
            interfaces: vec![
                uplink(10101, "Te1/0/1", "TenGigabitEthernet1/0/1", "downlink dist1", 10000, ("dist1", "Te1/1/1")),
                uplink(10102, "Te1/0/2", "TenGigabitEthernet1/0/2", "downlink dist2", 10000, ("dist2", "Te1/1/1")),
                uplink(10103, "Te1/0/3", "TenGigabitEthernet1/0/3", "firewall uplink", 10000, ("fw1", "port1")),
            ],
        },
        MockDevice {
            name: "dist1",
            model: "C9500-24Y4C",
            sys_descr: "Cisco IOS-XE Software (mock), Catalyst 9500 Switch",
            sw_rev: "17.9.4a",
            sensor_style: SensorStyle::Cisco,
            stp_vlans: &[10],
            vlan_style: VlanStyle::Cisco,
            vlans: &[1, 10],
            interfaces: vec![
                uplink(10101, "Te1/1/1", "TenGigabitEthernet1/1/1", "uplink core1", 10000, ("core1", "Te1/0/1")),
                uplink(10102, "Te1/1/2", "TenGigabitEthernet1/1/2", "downlink hall a 01", 10000, ("access-hall-a-01", "Te1/1/1")),
                MockInterface {
                    ifindex: 10103,
                    name: "Te1/1/3",
                    descr: "TenGigabitEthernet1/1/3",
                    alias: "downlink hall a 02",
                    speed_mbps: 10000,
                    peer: Some(("access-hall-a-02", "Te1/1/1")),
                    flaps: true,
                    up: true,
                },
            ],
        },
        MockDevice {
            name: "dist2",
            model: "C9500-24Y4C",
            sys_descr: "Cisco IOS-XE Software (mock), Catalyst 9500 Switch",
            sw_rev: "17.6.5",
            sensor_style: SensorStyle::Standard,
            stp_vlans: &[20],
            vlan_style: VlanStyle::Cisco,
            vlans: &[1, 20],
            interfaces: vec![
                uplink(10101, "Te1/1/1", "TenGigabitEthernet1/1/1", "uplink core1", 10000, ("core1", "Te1/0/2")),
                uplink(10102, "Te1/1/2", "TenGigabitEthernet1/1/2", "downlink hall b 01", 10000, ("access-hall-b-01", "Te1/1/1")),
                uplink(10104, "Te1/1/4", "TenGigabitEthernet1/1/4", "wlc uplink", 10000, ("wlc1", "Te0/0/1")),
            ],
        },
        access_switch("access-hall-a-01", ("dist1", "Te1/1/2"), false, VlanStyle::Cisco),
        access_switch("access-hall-a-02", ("dist1", "Te1/1/3"), true, VlanStyle::Cisco),
        // hall b answers only the standards-based Q-BRIDGE tables so the mock
        // exercises the vlanpoller's fallback path.
        access_switch("access-hall-b-01", ("dist2", "Te1/1/2"), false, VlanStyle::QBridge),
        MockDevice {
            name: "wlc1",
            model: "AIR-CT5520-K9",
            sys_descr: "Cisco 5520 Series Wireless LAN Controller (mock)",
            sw_rev: "8.10.185.0",
            sensor_style: SensorStyle::None,
            stp_vlans: &[],
            vlan_style: VlanStyle::None,
            vlans: &[],
            interfaces: vec![
                uplink(1, "Te0/0/1", "TenGigE0/0/1", "uplink dist2", 10000, ("dist2", "Te1/1/4")),
            ],
        },
        MockDevice {
            name: "fw1",
            model: "FGT-900D",
            sys_descr: "Mock firewall appliance",
            sw_rev: "7.2.8",
            sensor_style: SensorStyle::None,
            stp_vlans: &[],
            vlan_style: VlanStyle::None,
            vlans: &[],
            interfaces: vec![
                uplink(1, "port1", "port1", "uplink core1", 10000, ("core1", "Te1/0/3")),
            ],
        },
    ];
    Topology { devices, started: crate::utilities::tools::get_time() }
}

fn access_switch(name: &'static str, upstream: (&'static str, &'static str), uplink_flaps: bool, vlan_style: VlanStyle) -> MockDevice {
    let mut interfaces = vec![MockInterface {
        ifindex: 10101,
        name: "Te1/1/1",
        descr: "TenGigabitEthernet1/1/1",
        alias: "uplink",
        speed_mbps: 10000,
        peer: Some(upstream),
        flaps: uplink_flaps,
        up: true,
    }];
    // Eight access ports; a deterministic mix of up/down.
    const PORTS: [(&str, &str); 8] = [
        ("Gi1/0/1", "GigabitEthernet1/0/1"),
        ("Gi1/0/2", "GigabitEthernet1/0/2"),
        ("Gi1/0/3", "GigabitEthernet1/0/3"),
        ("Gi1/0/4", "GigabitEthernet1/0/4"),
        ("Gi1/0/5", "GigabitEthernet1/0/5"),
        ("Gi1/0/6", "GigabitEthernet1/0/6"),
        ("Gi1/0/7", "GigabitEthernet1/0/7"),
        ("Gi1/0/8", "GigabitEthernet1/0/8"),
    ];
    for (i, (name, descr)) in PORTS.iter().enumerate() {
        interfaces.push(access_port(10201 + i as i64, name, descr, i % 3 != 2));
    }
    MockDevice {
        name,
        model: "C9300-48P",
        sys_descr: "Cisco IOS-XE Software (mock), Catalyst 9300 Switch",
        sw_rev: "17.6.5",
        sensor_style: SensorStyle::Standard,
        stp_vlans: &[],
        vlan_style,
        vlans: &[1, 10, 20],
        interfaces,
    }
}

// ---------------------------------------------------------------------------
// Time-derived values (all monotonic or bounded; IMDS rejects regressions)
// ---------------------------------------------------------------------------

pub fn base_mac(dev_idx: usize) -> String {
    format!("02:00:00:00:{:02x}:01", 0x10 + dev_idx)
}

fn base_mac_spaced(dev_idx: usize) -> String {
    base_mac(dev_idx).replace(":", " ")
}

fn iface_mac(dev_idx: usize, ifindex: i64) -> String {
    format!("02:00:00:{:02x}:{:02x}:{:02x}", 0x10 + dev_idx, (ifindex / 100) as u8, (ifindex % 100) as u8)
}

// Trunks (peered interfaces) use native VLAN 1; access ports alternate
// between VLANs 10 and 20 by ifindex.
pub const TRUNK_NATIVE_VLAN: i64 = 1;

pub fn access_vlan(iface: &MockInterface) -> i64 {
    10 + (iface.ifindex % 2) * 10
}

// Inverse of collectors::vlanpoller::bitmap_values, in snmpbot's OCTET STRING
// rendering (lowercase space-separated hex): bit j of octet i (MSB first)
// represents base + i*8 + j.
pub fn hex_bitmap(values: &[i64], base: i64, len: usize) -> String {
    let mut octets = vec![0u8; len];
    for value in values.iter() {
        let offset = value - base;
        if offset >= 0 && (offset as usize) < len * 8 {
            octets[(offset / 8) as usize] |= 0x80u8 >> (offset % 8);
        }
    }
    octets.iter().map(|o| format!("{:02x}", o)).collect::<Vec<String>>().join(" ")
}

pub fn iface_up(iface: &MockInterface, elapsed: f64) -> bool {
    if iface.flaps {
        (elapsed.max(0.0) as u64 / FLAP_HALF_PERIOD_SECS) % 2 == 0
    } else {
        iface.up
    }
}

// Deterministic per-(device, interface, kind) rate so counters differ but
// never regress.
fn counter(dev_idx: usize, ifindex: i64, kind: u64, elapsed: f64) -> u64 {
    let seed = dev_idx as u64 * 131 + ifindex as u64 * 17 + kind * 7;
    let base = seed * 1_000_003;
    let rate = 5_000 + (seed * 37) % 45_000; // bytes or packets per second
    base + (rate as f64 * elapsed.max(0.0)) as u64
}

// A couple of ports accumulate errors slowly; everything else stays clean.
fn error_counter(dev_idx: usize, ifindex: i64, kind: u64, elapsed: f64) -> u64 {
    if (dev_idx as u64 + ifindex as u64 + kind) % 11 == 3 {
        (elapsed.max(0.0) as u64) / 60
    } else {
        0
    }
}

// Milli-celsius, bounded to roughly 39..45 °C.
pub fn sensor_value_milli(dev_idx: usize, sensor_idx: i64, elapsed: f64) -> u64 {
    let phase = dev_idx as f64 * 1.3 + sensor_idx as f64 * 0.7;
    (42_000.0 + 3_000.0 * (elapsed / 47.0 + phase).sin()) as u64
}

// ---------------------------------------------------------------------------
// Table / object generation
// ---------------------------------------------------------------------------

fn entry(index: serde_json::Value, objects: serde_json::Value) -> SNMPBotResultEntry {
    serde_json::from_value(json!({"HostID": "mock", "Index": index, "Objects": objects}))
        .expect("mock entry must match SNMPBotResultEntry shape")
}

fn response(id: &str, entries: Vec<SNMPBotResultEntry>) -> SNMPBotResponse {
    SNMPBotResponse {
        i_d: id.to_string(),
        index_keys: Vec::new(),
        object_keys: Vec::new(),
        entries,
    }
}

impl Topology {
    fn device_by_fqdn(&self, fqdn: &str) -> Option<(usize, &MockDevice)> {
        let name = fqdn.strip_suffix(&format!(".{}", DOMAIN))?;
        self.devices.iter().enumerate().find(|(_, d)| d.name == name)
    }

    // LLDP neighbors of `dev`: its own declared peers plus links declared on
    // the remote side pointing back at it. (local ifindex, remote device idx,
    // remote interface name)
    fn neighbors(&self, dev: &MockDevice) -> Vec<(i64, usize, &'static str)> {
        let mut out: Vec<(i64, usize, &'static str)> = Vec::new();
        for iface in dev.interfaces.iter() {
            if let Some((peer_name, peer_iface)) = iface.peer {
                if let Some((peer_idx, _)) = self.devices.iter().enumerate().find(|(_, d)| d.name == peer_name) {
                    out.push((iface.ifindex, peer_idx, peer_iface));
                }
            }
        }
        for (other_idx, other) in self.devices.iter().enumerate() {
            if other.name == dev.name {
                continue;
            }
            for other_iface in other.interfaces.iter() {
                if let Some((peer_name, peer_iface)) = other_iface.peer {
                    if peer_name == dev.name {
                        if let Some(local) = dev.interfaces.iter().find(|i| i.name == peer_iface) {
                            if !out.iter().any(|(ifidx, oidx, _)| *ifidx == local.ifindex && *oidx == other_idx) {
                                out.push((local.ifindex, other_idx, other_iface.name));
                            }
                        }
                    }
                }
            }
        }
        out
    }

    // STP member ports of a vlan: every peered interface of the device.
    fn stp_ports(dev: &MockDevice) -> Vec<(i64, &MockInterface)> {
        dev.interfaces
            .iter()
            .filter(|i| i.peer.is_some())
            .enumerate()
            .map(|(pos, iface)| (pos as i64 + 1, iface))
            .collect()
    }

    // Q-BRIDGE bridge ports: every interface, numbered by position. Distinct
    // from stp_ports (peered only) — VLAN membership covers access ports too.
    fn bridge_ports(dev: &MockDevice) -> Vec<(i64, &MockInterface)> {
        dev.interfaces
            .iter()
            .enumerate()
            .map(|(pos, iface)| (pos as i64 + 1, iface))
            .collect()
    }

    pub fn table(&self, fqdn: &str, vlan: Option<i64>, table_id: &str, elapsed: f64) -> Option<SNMPBotResponse> {
        let (dev_idx, dev) = self.device_by_fqdn(fqdn)?;
        match table_id {
            "IF-MIB::ifTable" => {
                let entries = dev.interfaces.iter().map(|iface| {
                    entry(
                        json!({"IF-MIB::ifIndex": iface.ifindex}),
                        json!({
                            "IF-MIB::ifIndex": iface.ifindex,
                            "IF-MIB::ifDescr": iface.descr,
                            "IF-MIB::ifType": "ethernetCsmacd",
                            "IF-MIB::ifPhysAddress": iface_mac(dev_idx, iface.ifindex),
                            "IF-MIB::ifOperStatus": if iface_up(iface, elapsed) { "up" } else { "down" },
                            "IF-MIB::ifInErrors": error_counter(dev_idx, iface.ifindex, 1, elapsed),
                            "IF-MIB::ifOutErrors": error_counter(dev_idx, iface.ifindex, 2, elapsed),
                            "IF-MIB::ifOutDiscards": error_counter(dev_idx, iface.ifindex, 3, elapsed),
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "IF-MIB::ifXTable" => {
                let entries = dev.interfaces.iter().map(|iface| {
                    let c = |kind: u64| counter(dev_idx, iface.ifindex, kind, elapsed);
                    entry(
                        json!({"IF-MIB::ifIndex": iface.ifindex}),
                        json!({
                            "IF-MIB::ifName": iface.name,
                            "IF-MIB::ifAlias": iface.alias,
                            "IF-MIB::ifHighSpeed": iface.speed_mbps,
                            "IF-MIB::ifHCInOctets": c(10),
                            "IF-MIB::ifHCOutOctets": c(11),
                            "IF-MIB::ifHCInUcastPkts": c(12),
                            "IF-MIB::ifHCInMulticastPkts": c(13),
                            "IF-MIB::ifHCInBroadcastPkts": c(14),
                            "IF-MIB::ifHCOutUcastPkts": c(15),
                            "IF-MIB::ifHCOutMulticastPkts": c(16),
                            "IF-MIB::ifHCOutBroadcastPkts": c(17),
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "ENTITY-MIB::entPhysicalTable" => {
                // Chassis entry (discovery: device_type + software_version)...
                let mut entries = vec![entry(
                    json!({"ENTITY-MIB::entPhysicalIndex": 1}),
                    json!({
                        "ENTITY-MIB::entPhysicalClass": "chassis",
                        "ENTITY-MIB::entPhysicalDescr": dev.sys_descr,
                        "ENTITY-MIB::entPhysicalModelName": dev.model,
                        "ENTITY-MIB::entPhysicalSoftwareRev": dev.sw_rev,
                    }),
                )];
                // ...plus sensor identities (entitypoller). Sensor names lead
                // with the interface name so the sensor-to-interface
                // association resolves.
                for (sensor_idx, iface) in dev.interfaces.iter().filter(|i| i.peer.is_some()).enumerate() {
                    entries.push(entry(
                        json!({"ENTITY-MIB::entPhysicalIndex": 1001 + sensor_idx as i64}),
                        json!({
                            "ENTITY-MIB::entPhysicalName": format!("{} Temperature Sensor", iface.name),
                            "ENTITY-MIB::entPhysicalDescr": "Transceiver temperature sensor",
                        }),
                    ));
                }
                entries.push(entry(
                    json!({"ENTITY-MIB::entPhysicalIndex": 2001}),
                    json!({
                        "ENTITY-MIB::entPhysicalName": "PSU 1",
                        "ENTITY-MIB::entPhysicalDescr": "Power supply sensor",
                    }),
                ));
                Some(response(table_id, entries))
            }
            "ENTITY-SENSOR-MIB::entPhySensorTable" | "CISCO-ENTITY-SENSOR-MIB::entSensorValueTable" => {
                let style = if table_id.starts_with("CISCO") { SensorStyle::Cisco } else { SensorStyle::Standard };
                if dev.sensor_style != style {
                    return None;
                }
                let (value_key, scale_key, precision_key, type_key) = if style == SensorStyle::Cisco {
                    ("CISCO-ENTITY-SENSOR-MIB::entSensorValue", "CISCO-ENTITY-SENSOR-MIB::entSensorScale",
                     "CISCO-ENTITY-SENSOR-MIB::entSensorPrecision", "CISCO-ENTITY-SENSOR-MIB::entSensorType")
                } else {
                    ("ENTITY-SENSOR-MIB::entPhySensorValue", "ENTITY-SENSOR-MIB::entPhySensorScale",
                     "ENTITY-SENSOR-MIB::entPhySensorPrecision", "ENTITY-SENSOR-MIB::entPhySensorType")
                };
                let uplink_count = dev.interfaces.iter().filter(|i| i.peer.is_some()).count() as i64;
                let mut entries = Vec::new();
                for sensor_idx in 0..uplink_count {
                    entries.push(entry(
                        json!({"ENTITY-MIB::entPhysicalIndex": 1001 + sensor_idx}),
                        json!({
                            value_key: sensor_value_milli(dev_idx, sensor_idx, elapsed),
                            scale_key: "milli",
                            precision_key: 0,
                            type_key: "celsius",
                        }),
                    ));
                }
                entries.push(entry(
                    json!({"ENTITY-MIB::entPhysicalIndex": 2001}),
                    json!({
                        value_key: 12_100 + (dev_idx as u64 * 13) % 200,
                        scale_key: "milli",
                        precision_key: 0,
                        type_key: "voltsDC",
                    }),
                ));
                Some(response(table_id, entries))
            }
            "CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleTable" => {
                if dev.stp_vlans.is_empty() {
                    return None;
                }
                let mut entries = Vec::new();
                for vlan in dev.stp_vlans.iter() {
                    for (bridge_port, iface) in Self::stp_ports(dev) {
                        // The core is the root bridge: everything designated.
                        // Distribution: uplink to core is the root port; one
                        // downlink plays alternate for variety.
                        let role = if dev.name == "core1" {
                            "designated"
                        } else if iface.alias.contains("uplink core1") {
                            "root"
                        } else if iface.flaps {
                            "alternate"
                        } else {
                            "designated"
                        };
                        entries.push(entry(
                            json!({
                                "CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleInstanceIndex": vlan,
                                "CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRolePortIndex": bridge_port,
                            }),
                            json!({"CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleValue": role}),
                        ));
                    }
                }
                Some(response(table_id, entries))
            }
            "BRIDGE-MIB::dot1dBasePortTable" => {
                // Per-VLAN community form (entitypoller STP): STP member ports
                // only. Base community form (vlanpoller Q-BRIDGE fallback):
                // every bridge port, answered only by QBridge-style devices.
                let ports = match vlan {
                    Some(vlan) => {
                        if !dev.stp_vlans.contains(&vlan) {
                            return None;
                        }
                        Self::stp_ports(dev)
                    }
                    None => {
                        if dev.vlan_style != VlanStyle::QBridge {
                            return None;
                        }
                        Self::bridge_ports(dev)
                    }
                };
                let entries = ports.into_iter().map(|(bridge_port, iface)| {
                    entry(
                        json!({"BRIDGE-MIB::dot1dBasePort": bridge_port}),
                        json!({"BRIDGE-MIB::dot1dBasePortIfIndex": iface.ifindex}),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "CISCO-VTP-MIB::jaspyVlanTrunkPortTable" => {
                if dev.vlan_style != VlanStyle::Cisco {
                    return None;
                }
                // Like real Cisco gear with the default "allowed vlan all"
                // config: the enabled bitmaps report every VLAN, and the
                // collector must intersect with vtpVlanTable to get the truth.
                let all_1k = hex_bitmap(&(1..1024).collect::<Vec<i64>>(), 0, 128);
                let all_2k: String = hex_bitmap(&(1024..2048).collect::<Vec<i64>>(), 1024, 128);
                let entries = dev.interfaces.iter().map(|iface| {
                    let trunking = if iface.peer.is_some() { "trunking" } else { "notTrunking" };
                    entry(
                        json!({"CISCO-VTP-MIB::vlanTrunkPortIfIndex": iface.ifindex}),
                        json!({
                            "CISCO-VTP-MIB::vlanTrunkPortDynamicStatus": trunking,
                            "CISCO-VTP-MIB::vlanTrunkPortNativeVlan": TRUNK_NATIVE_VLAN,
                            "CISCO-VTP-MIB::vlanTrunkPortVlansEnabled": all_1k,
                            "CISCO-VTP-MIB::vlanTrunkPortVlansEnabled2k": all_2k,
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "CISCO-VTP-MIB::vtpVlanTable" => {
                if dev.vlan_style != VlanStyle::Cisco {
                    return None;
                }
                let entries = dev.vlans.iter().map(|vlan| {
                    entry(
                        json!({"CISCO-VTP-MIB::managementDomainIndex": 1, "CISCO-VTP-MIB::vtpVlanIndex": vlan}),
                        json!({
                            "CISCO-VTP-MIB::vtpVlanState": "operational",
                            "CISCO-VTP-MIB::vtpVlanType": "ethernet",
                            "CISCO-VTP-MIB::vtpVlanName": format!("mock-vlan-{}", vlan),
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "CISCO-VLAN-MEMBERSHIP-MIB::vmMembershipTable" => {
                if dev.vlan_style != VlanStyle::Cisco {
                    return None;
                }
                let entries = dev.interfaces.iter().filter(|i| i.peer.is_none()).map(|iface| {
                    entry(
                        json!({"IF-MIB::ifIndex": iface.ifindex}),
                        json!({
                            "CISCO-VLAN-MEMBERSHIP-MIB::vmVlanType": "static",
                            "CISCO-VLAN-MEMBERSHIP-MIB::vmVlan": access_vlan(iface),
                            "CISCO-VLAN-MEMBERSHIP-MIB::vmPortStatus": "active",
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "Q-BRIDGE-MIB::dot1qPortVlanTable" => {
                if dev.vlan_style != VlanStyle::QBridge {
                    return None;
                }
                let entries = Self::bridge_ports(dev).into_iter().map(|(bridge_port, iface)| {
                    let pvid = if iface.peer.is_some() { TRUNK_NATIVE_VLAN } else { access_vlan(iface) };
                    entry(
                        json!({"BRIDGE-MIB::dot1dBasePort": bridge_port}),
                        json!({"Q-BRIDGE-MIB::dot1qPvid": pvid}),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "Q-BRIDGE-MIB::dot1qVlanCurrentTable" => {
                if dev.vlan_style != VlanStyle::QBridge {
                    return None;
                }
                // Per VLAN: trunks carry every VLAN (untagged only on their
                // native), access ports appear untagged on their own VLAN.
                let entries = dev.vlans.iter().map(|vlan| {
                    let mut egress: Vec<i64> = Vec::new();
                    let mut untagged: Vec<i64> = Vec::new();
                    for (bridge_port, iface) in Self::bridge_ports(dev) {
                        if iface.peer.is_some() {
                            egress.push(bridge_port);
                            if *vlan == TRUNK_NATIVE_VLAN {
                                untagged.push(bridge_port);
                            }
                        } else if access_vlan(iface) == *vlan {
                            egress.push(bridge_port);
                            untagged.push(bridge_port);
                        }
                    }
                    let len = (dev.interfaces.len() + 7) / 8;
                    entry(
                        json!({"Q-BRIDGE-MIB::dot1qVlanTimeMark": 0, "Q-BRIDGE-MIB::dot1qVlanIndex": vlan}),
                        json!({
                            "Q-BRIDGE-MIB::dot1qVlanCurrentEgressPorts": hex_bitmap(&egress, 1, len),
                            "Q-BRIDGE-MIB::dot1qVlanCurrentUntaggedPorts": hex_bitmap(&untagged, 1, len),
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "BRIDGE-MIB::dot1dStpPortTable" => {
                let vlan = vlan?;
                if !dev.stp_vlans.contains(&vlan) {
                    return None;
                }
                let entries = Self::stp_ports(dev).into_iter().map(|(bridge_port, iface)| {
                    let root_port = iface.alias.contains("uplink core1");
                    let alternate = iface.flaps;
                    entry(
                        json!({"BRIDGE-MIB::dot1dStpPort": bridge_port}),
                        json!({
                            "BRIDGE-MIB::dot1dStpPortDesignatedCost": if dev.name == "core1" { 0 } else { 4 },
                            "BRIDGE-MIB::dot1dStpPortPathCost": if root_port { 4 } else { 19 },
                            "BRIDGE-MIB::dot1dStpPortPriority": 128,
                            "BRIDGE-MIB::dot1dStpPortForwardTransitions": 1,
                            "BRIDGE-MIB::dot1dStpPortEnable": "enabled",
                            "BRIDGE-MIB::dot1dStpPortState": if alternate { "blocking" } else { "forwarding" },
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "LLDP-MIB::lldpLocPortTable" => {
                let entries = dev.interfaces.iter().map(|iface| {
                    entry(
                        json!({"LLDP-MIB::lldpLocPortNum": iface.ifindex}),
                        json!({
                            "LLDP-MIB::lldpLocPortIdSubtype": "interfaceName",
                            "LLDP-MIB::lldpLocPortId": iface.name,
                            "LLDP-MIB::lldpLocPortDesc": iface.descr,
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "LLDP-MIB::lldpRemTable" => {
                // LLDP caches survive short link flaps, so adjacency is
                // emitted unconditionally: discovery resolves links even when
                // the flapping uplink happens to be down.
                let entries = self.neighbors(dev).into_iter().map(|(local_ifindex, peer_idx, peer_iface)| {
                    entry(
                        json!({"LLDP-MIB::lldpRemLocalPortNum": local_ifindex}),
                        json!({
                            "LLDP-MIB::lldpRemSysName": self.devices[peer_idx].name,
                            "LLDP-MIB::lldpRemChassisId": base_mac_spaced(peer_idx),
                            "LLDP-MIB::lldpRemPortId": peer_iface,
                            "LLDP-MIB::lldpRemPortIdSubtype": "interfaceName",
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            // Valid-but-empty keeps the discovery log free of CDP noise.
            "CISCO-CDP-MIB::cdpCacheTable" => Some(response(table_id, Vec::new())),
            _ => None,
        }
    }

    pub fn object(&self, fqdn: &str, object_id: &str) -> Option<serde_json::Value> {
        let (dev_idx, dev) = self.device_by_fqdn(fqdn)?;
        match object_id {
            "SNMPv2-MIB::sysDescr" => Some(json!(dev.sys_descr)),
            "BRIDGE-MIB::dot1dBaseBridgeAddress" => Some(json!(base_mac_spaced(dev_idx))),
            "LLDP-MIB::lldpLocChassisId" => Some(json!(base_mac_spaced(dev_idx))),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::poller::SNMPBotResultEntryObjectValue;

    const ALL_TABLES: [&str; 14] = [
        "IF-MIB::ifTable",
        "IF-MIB::ifXTable",
        "ENTITY-MIB::entPhysicalTable",
        "ENTITY-SENSOR-MIB::entPhySensorTable",
        "CISCO-ENTITY-SENSOR-MIB::entSensorValueTable",
        "CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleTable",
        "LLDP-MIB::lldpLocPortTable",
        "LLDP-MIB::lldpRemTable",
        "CISCO-CDP-MIB::cdpCacheTable",
        "CISCO-VTP-MIB::jaspyVlanTrunkPortTable",
        "CISCO-VTP-MIB::vtpVlanTable",
        "CISCO-VLAN-MEMBERSHIP-MIB::vmMembershipTable",
        "Q-BRIDGE-MIB::dot1qPortVlanTable",
        "Q-BRIDGE-MIB::dot1qVlanCurrentTable",
    ];

    #[test]
    fn tables_serialize_with_snmpbot_shape() {
        let topo = build();
        for dev in topo.devices.iter() {
            for table in ALL_TABLES.iter() {
                if let Some(resp) = topo.table(&dev.fqdn(), Some(10), table, 30.0) {
                    let value = serde_json::to_value(&resp).unwrap();
                    assert!(value.get("ID").is_some(), "{} missing PascalCase ID", table);
                    assert!(value.get("Entries").is_some(), "{} missing Entries", table);
                    // Round-trip through the exact structs the collectors parse.
                    let parsed: crate::collectors::poller::SNMPBotResponse =
                        serde_json::from_value(value).unwrap();
                    for entry in parsed.entries.iter() {
                        assert!(!entry.index.is_empty(), "{} entry without Index", table);
                    }
                }
            }
        }
    }

    #[test]
    fn every_device_answers_the_critical_tables() {
        let topo = build();
        for dev in topo.devices.iter() {
            for table in ["IF-MIB::ifTable", "IF-MIB::ifXTable"] {
                let resp = topo.table(&dev.fqdn(), None, table, 0.0).unwrap();
                assert!(!resp.entries.is_empty(), "{} {} must be non-empty (device-up signal)", dev.name, table);
            }
        }
    }

    #[test]
    fn counters_are_monotonic() {
        for (elapsed_early, elapsed_late) in [(0.0, 1.0), (59.9, 60.1), (100.0, 1000.0)] {
            for kind in [10, 11, 15, 1] {
                let early = counter(3, 10101, kind, elapsed_early);
                let late = counter(3, 10101, kind, elapsed_late);
                assert!(early <= late, "counter regressed for kind {}", kind);
            }
        }
        // Uplink octets must actually grow over a poll interval.
        assert!(counter(0, 10101, 10, 60.0) > counter(0, 10101, 10, 0.0));
        // Error counters are monotonic too.
        assert!(error_counter(3, 10201, 1, 600.0) >= error_counter(3, 10201, 1, 60.0));
    }

    #[test]
    fn flap_function_has_expected_period() {
        let topo = build();
        let dist1 = topo.devices.iter().find(|d| d.name == "dist1").unwrap();
        let flapping = dist1.interfaces.iter().find(|i| i.flaps).unwrap();
        assert!(iface_up(flapping, 0.0));
        assert!(iface_up(flapping, 59.0));
        assert!(!iface_up(flapping, 60.0));
        assert!(!iface_up(flapping, 119.0));
        assert!(iface_up(flapping, 120.0));

        // Both ends of the flapping link agree at all times.
        let a02 = topo.devices.iter().find(|d| d.name == "access-hall-a-02").unwrap();
        let other_end = a02.interfaces.iter().find(|i| i.flaps).unwrap();
        for t in [0.0, 30.0, 61.0, 90.0, 121.0] {
            assert_eq!(iface_up(flapping, t), iface_up(other_end, t), "flap ends disagree at t={}", t);
        }
    }

    #[test]
    fn non_flapping_interfaces_hold_static_state() {
        let topo = build();
        let a01 = topo.devices.iter().find(|d| d.name == "access-hall-a-01").unwrap();
        for iface in a01.interfaces.iter().filter(|i| !i.flaps) {
            assert_eq!(iface_up(iface, 0.0), iface_up(iface, 3600.0));
        }
        // The static mix contains both up and down access ports.
        assert!(a01.interfaces.iter().any(|i| i.peer.is_none() && i.up));
        assert!(a01.interfaces.iter().any(|i| i.peer.is_none() && !i.up));
    }

    #[test]
    fn lldp_adjacency_is_reciprocal_and_resolvable() {
        let topo = build();
        for dev in topo.devices.iter() {
            for iface in dev.interfaces.iter() {
                if let Some((peer_name, peer_iface)) = iface.peer {
                    let peer = topo.devices.iter().find(|d| d.name == peer_name)
                        .unwrap_or_else(|| panic!("{}: peer {} not in topology", dev.name, peer_name));
                    assert!(
                        peer.interfaces.iter().any(|i| i.name == peer_iface),
                        "{}: peer interface {}:{} missing", dev.name, peer_name, peer_iface
                    );
                }
            }
            // Every neighbor visible in the LLDP rem table maps back: the
            // remote device sees this device on the named interface.
            for (local_ifindex, peer_idx, peer_iface) in topo.neighbors(dev) {
                let peer = &topo.devices[peer_idx];
                let reciprocal = topo.neighbors(peer);
                let local_iface = dev.interfaces.iter().find(|i| i.ifindex == local_ifindex).unwrap();
                assert!(
                    reciprocal.iter().any(|(rem_ifindex, rem_peer_idx, rem_peer_iface)| {
                        topo.devices[*rem_peer_idx].name == dev.name
                            && *rem_peer_iface == local_iface.name
                            && peer.interfaces.iter().any(|i| i.ifindex == *rem_ifindex && i.name == peer_iface)
                    }),
                    "asymmetric link {}:{} -> {}:{}", dev.name, local_iface.name, peer.name, peer_iface
                );
            }
        }
    }

    #[test]
    fn base_macs_are_unique() {
        let topo = build();
        let macs: std::collections::HashSet<String> =
            (0..topo.devices.len()).map(base_mac).collect();
        assert_eq!(macs.len(), topo.devices.len());
    }

    #[test]
    fn sensor_values_stay_in_bounds() {
        for dev_idx in 0..8 {
            for sensor_idx in 0..3 {
                for t in [0.0, 10.0, 100.0, 1000.0, 86_400.0] {
                    let milli = sensor_value_milli(dev_idx, sensor_idx, t);
                    assert!((38_000..=46_000).contains(&milli), "sensor out of range: {}", milli);
                }
            }
        }
    }

    #[test]
    fn sensor_style_gates_the_two_sensor_tables() {
        let topo = build();
        let cisco = topo.devices.iter().find(|d| d.sensor_style == SensorStyle::Cisco).unwrap();
        let standard = topo.devices.iter().find(|d| d.sensor_style == SensorStyle::Standard).unwrap();
        let none = topo.devices.iter().find(|d| d.sensor_style == SensorStyle::None).unwrap();
        let phy = "ENTITY-SENSOR-MIB::entPhySensorTable";
        let cisco_t = "CISCO-ENTITY-SENSOR-MIB::entSensorValueTable";
        assert!(topo.table(&cisco.fqdn(), None, cisco_t, 0.0).is_some());
        assert!(topo.table(&cisco.fqdn(), None, phy, 0.0).is_none());
        assert!(topo.table(&standard.fqdn(), None, phy, 0.0).is_some());
        assert!(topo.table(&standard.fqdn(), None, cisco_t, 0.0).is_none());
        assert!(topo.table(&none.fqdn(), None, phy, 0.0).is_none());
        assert!(topo.table(&none.fqdn(), None, cisco_t, 0.0).is_none());
    }

    #[test]
    fn vlan_style_gates_the_vlan_tables() {
        let topo = build();
        let cisco = topo.devices.iter().find(|d| d.vlan_style == VlanStyle::Cisco).unwrap();
        let qbridge = topo.devices.iter().find(|d| d.vlan_style == VlanStyle::QBridge).unwrap();
        let none = topo.devices.iter().find(|d| d.vlan_style == VlanStyle::None).unwrap();
        let cisco_tables = ["CISCO-VTP-MIB::jaspyVlanTrunkPortTable", "CISCO-VTP-MIB::vtpVlanTable", "CISCO-VLAN-MEMBERSHIP-MIB::vmMembershipTable"];
        let qbridge_tables = ["Q-BRIDGE-MIB::dot1qPortVlanTable", "Q-BRIDGE-MIB::dot1qVlanCurrentTable"];
        for table in cisco_tables {
            assert!(topo.table(&cisco.fqdn(), None, table, 0.0).is_some(), "{} must answer on cisco", table);
            assert!(topo.table(&qbridge.fqdn(), None, table, 0.0).is_none(), "{} must 404 on qbridge", table);
            assert!(topo.table(&none.fqdn(), None, table, 0.0).is_none(), "{} must 404 on none", table);
        }
        for table in qbridge_tables {
            assert!(topo.table(&qbridge.fqdn(), None, table, 0.0).is_some(), "{} must answer on qbridge", table);
            assert!(topo.table(&cisco.fqdn(), None, table, 0.0).is_none(), "{} must 404 on cisco", table);
            assert!(topo.table(&none.fqdn(), None, table, 0.0).is_none(), "{} must 404 on none", table);
        }
        // The base-community dot1dBasePortTable form (no vlan) is the
        // Q-BRIDGE translation table and only answers for QBridge devices;
        // the per-vlan STP form keeps working for everyone with stp_vlans.
        assert!(topo.table(&qbridge.fqdn(), None, "BRIDGE-MIB::dot1dBasePortTable", 0.0).is_some());
        assert!(topo.table(&cisco.fqdn(), None, "BRIDGE-MIB::dot1dBasePortTable", 0.0).is_none());
        let stp_dev = topo.devices.iter().find(|d| !d.stp_vlans.is_empty()).unwrap();
        assert!(topo.table(&stp_dev.fqdn(), Some(stp_dev.stp_vlans[0]), "BRIDGE-MIB::dot1dBasePortTable", 0.0).is_some());
    }

    // The generated tables must decode through the real vlanpoller logic:
    // trunks native 1 + tagged = device vlans minus 1, access ports on their
    // access_vlan. Guards the mock and the decoder against drifting apart.
    #[test]
    fn vlan_tables_decode_via_vlanpoller() {
        use crate::collectors::vlanpoller::{decode_cisco, decode_qbridge};
        let topo = build();

        let cisco = topo.devices.iter().find(|d| d.name == "access-hall-a-01").unwrap();
        let trunk = topo.table(&cisco.fqdn(), None, "CISCO-VTP-MIB::jaspyVlanTrunkPortTable", 0.0).unwrap();
        let vtp = topo.table(&cisco.fqdn(), None, "CISCO-VTP-MIB::vtpVlanTable", 0.0).unwrap();
        let membership = topo.table(&cisco.fqdn(), None, "CISCO-VLAN-MEMBERSHIP-MIB::vmMembershipTable", 0.0).unwrap();
        let decoded = decode_cisco(&trunk, Some(&vtp), Some(&membership));
        for iface in cisco.interfaces.iter() {
            let port = &decoded[&iface.ifindex];
            if iface.peer.is_some() {
                assert_eq!(port.native_vlan, Some(TRUNK_NATIVE_VLAN), "{} trunk native", iface.name);
                assert_eq!(port.tagged_vlans, vec![10, 20], "{} trunk tagged", iface.name);
            } else {
                assert_eq!(port.native_vlan, Some(access_vlan(iface)), "{} access vlan", iface.name);
                assert!(port.tagged_vlans.is_empty(), "{} access tagged", iface.name);
            }
        }

        let qbridge = topo.devices.iter().find(|d| d.vlan_style == VlanStyle::QBridge).unwrap();
        let pvid = topo.table(&qbridge.fqdn(), None, "Q-BRIDGE-MIB::dot1qPortVlanTable", 0.0).unwrap();
        let current = topo.table(&qbridge.fqdn(), None, "Q-BRIDGE-MIB::dot1qVlanCurrentTable", 0.0).unwrap();
        let base = topo.table(&qbridge.fqdn(), None, "BRIDGE-MIB::dot1dBasePortTable", 0.0).unwrap();
        let decoded = decode_qbridge(Some(&pvid), Some(&current), &base);
        for iface in qbridge.interfaces.iter() {
            let port = &decoded[&iface.ifindex];
            if iface.peer.is_some() {
                assert_eq!(port.native_vlan, Some(TRUNK_NATIVE_VLAN), "{} trunk native", iface.name);
                assert_eq!(port.tagged_vlans, vec![10, 20], "{} trunk tagged", iface.name);
            } else {
                assert_eq!(port.native_vlan, Some(access_vlan(iface)), "{} access vlan", iface.name);
                assert!(port.tagged_vlans.is_empty(), "{} access tagged", iface.name);
            }
        }
    }

    #[test]
    fn hex_bitmap_roundtrips_through_vlanpoller_decoder() {
        use crate::collectors::vlanpoller::{bitmap_values, parse_hex_octets};
        for (values, base, len) in [
            (vec![1i64, 10, 20], 0i64, 128usize),
            (vec![1, 5, 6], 1, 4),
            (vec![1024, 1100], 1024, 128),
            (Vec::new(), 0, 8),
        ] {
            let encoded = hex_bitmap(&values, base, len);
            assert_eq!(bitmap_values(&parse_hex_octets(&encoded), base), values, "roundtrip {:?}", values);
        }
    }

    #[test]
    fn stp_bridge_ports_all_resolve_via_base_port_table() {
        let topo = build();
        for dev in topo.devices.iter().filter(|d| !d.stp_vlans.is_empty()) {
            for vlan in dev.stp_vlans.iter() {
                let roles = topo.table(&dev.fqdn(), None, "CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleTable", 0.0).unwrap();
                let base = topo.table(&dev.fqdn(), Some(*vlan), "BRIDGE-MIB::dot1dBasePortTable", 0.0).unwrap();
                let stp = topo.table(&dev.fqdn(), Some(*vlan), "BRIDGE-MIB::dot1dStpPortTable", 0.0).unwrap();
                let base_ports: std::collections::HashSet<i64> =
                    base.entries.iter().map(|e| e.index["BRIDGE-MIB::dot1dBasePort"]).collect();
                for entry in roles.entries.iter().filter(|e| e.index["CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleInstanceIndex"] == *vlan) {
                    let port = entry.index["CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRolePortIndex"];
                    assert!(base_ports.contains(&port), "{} vlan {} port {} missing from base table", dev.name, vlan, port);
                }
                assert_eq!(base.entries.len(), stp.entries.len());
            }
        }
    }

    #[test]
    fn objects_answer_for_every_device() {
        let topo = build();
        for (idx, dev) in topo.devices.iter().enumerate() {
            let mac = topo.object(&dev.fqdn(), "BRIDGE-MIB::dot1dBaseBridgeAddress").unwrap();
            assert_eq!(mac.as_str().unwrap(), base_mac_spaced(idx));
            assert!(topo.object(&dev.fqdn(), "SNMPv2-MIB::sysDescr").is_some());
            assert!(topo.object(&dev.fqdn(), "LLDP-MIB::lldpLocChassisId").is_some());
            assert!(topo.object(&dev.fqdn(), "NO-SUCH-MIB::thing").is_none());
        }
        assert!(topo.object("ghost.mock.jaspy", "SNMPv2-MIB::sysDescr").is_none());
    }

    #[test]
    fn ifoperstatus_values_are_strings() {
        let topo = build();
        let resp = topo.table(ROOT_DEVICE, None, "IF-MIB::ifTable", 0.0).unwrap();
        for entry in resp.entries.iter() {
            match entry.objects.get("IF-MIB::ifOperStatus") {
                Some(SNMPBotResultEntryObjectValue::Str(s)) => assert!(s == "up" || s == "down"),
                other => panic!("ifOperStatus must be a string, got {:?}", other.is_some()),
            }
        }
    }
}
