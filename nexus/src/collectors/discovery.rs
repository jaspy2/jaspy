// In-process topology discovery engine (formerly the standalone Python
// `discover` tool). Ported near-verbatim from discover/discover.py and
// discover/lib/SNMPDataSource.py.
//
// A supervisor thread waits for a manual trigger (POST /dev/discovery/run) or a
// periodic interval, then crawls the network from the configured root device by
// following LLDP/CDP neighbor announcements via snmpbot, and ingests devices
// and links through the same utilities::discovery functions the HTTP PUT
// endpoints use — no localhost HTTP round-trip.
extern crate reqwest;
extern crate serde_json;

use crate::collectors::poller::{SNMPBotResponse, SNMPBotResultEntry, SNMPBotResultEntryObjectValue};
use crate::db;
use crate::models;
use crate::utilities;
use crate::utilities::msgbus::MessageBus;
use crate::utilities::cache::CacheController;
use std::collections::{HashMap, HashSet};
use std::sync::{atomic, Arc, Mutex};
use std::thread;
use std::time;

// ---------------------------------------------------------------------------
// Control state shared with the HTTP routes (POST /run, GET /status, config)
// ---------------------------------------------------------------------------

pub struct DiscoveryControl {
    pub config: models::json::DiscoveryConfig,
    pub status: models::json::DiscoveryStatus,
    pub trigger_requested: bool,
    pub trigger_overrides: Option<models::json::DiscoveryRunRequest>,
}

impl DiscoveryControl {
    pub fn new(config: models::json::DiscoveryConfig) -> DiscoveryControl {
        DiscoveryControl {
            config: config,
            status: models::json::DiscoveryStatus::default(),
            trigger_requested: false,
            trigger_overrides: None,
        }
    }

    pub fn status_dto(&self) -> models::json::DiscoveryStatus {
        self.status.clone()
    }
}

#[derive(Clone)]
struct RunParams {
    snmpbot_url: String,
    root_device: String,
    community: String,
    dns_domains: Vec<String>,
    ignore: Vec<String>,
    remap: HashMap<String, String>,
    topology_stable: bool,
    skip_dns: bool,
}

// ---------------------------------------------------------------------------
// snmpbot access (tables reuse the poller structs; objects endpoint is new)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct SNMPBotObjectInstance {
    #[serde(default)]
    value: Option<SNMPBotResultEntryObjectValue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct SNMPBotObjectResponse {
    #[allow(dead_code)]
    i_d: String,
    #[serde(default, deserialize_with = "crate::collectors::poller::null_to_default")]
    instances: Vec<SNMPBotObjectInstance>,
}

fn snmpbot_url_for(snmpbot_url: &str, fqdn: &str, community: &str, kind: &str, id: &str) -> Option<reqwest::Url> {
    let source = format!("{}/api/hosts/{}/{}/{}", snmpbot_url, fqdn, kind, id);
    if let Ok(mut parsed) = reqwest::Url::parse(&source) {
        parsed.query_pairs_mut().append_pair("snmp", &format!("{}@{}", community, fqdn));
        Some(parsed)
    } else {
        None
    }
}

fn fetch_table(client: &reqwest::blocking::Client, snmpbot_url: &str, fqdn: &str, community: &str, table: &str) -> Result<SNMPBotResponse, String> {
    let url = snmpbot_url_for(snmpbot_url, fqdn, community, "tables", table).ok_or("bad url")?;
    let response = client.get(url).send().map_err(|e| format!("{}", e))?;
    if !response.status().is_success() {
        return Err(format!("status={}", response.status()));
    }
    let body = response.text().map_err(|e| format!("read: {}", e))?;
    serde_json::from_str(&body).map_err(|e| format!("json: {} (body: {:.200})", e, body))
}

fn fetch_object(client: &reqwest::blocking::Client, snmpbot_url: &str, fqdn: &str, community: &str, object: &str) -> Option<String> {
    let url = snmpbot_url_for(snmpbot_url, fqdn, community, "objects", object)?;
    let response = client.get(url).send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    let parsed: SNMPBotObjectResponse = match response.json() {
        Ok(p) => p,
        Err(e) => {
            println!("[discovery] [{}] error decoding object {}: {}", fqdn, object, e);
            return None;
        }
    };
    if parsed.instances.len() > 1 {
        println!("[discovery] [{}] expected <= 1 results for {}, got {}", fqdn, object, parsed.instances.len());
        return None;
    }
    match parsed.instances.into_iter().next()?.value? {
        SNMPBotResultEntryObjectValue::Str(s) => Some(s),
        SNMPBotResultEntryObjectValue::Uint64(v) => Some(format!("{}", v)),
        SNMPBotResultEntryObjectValue::Float64(v) => Some(format!("{}", v)),
        SNMPBotResultEntryObjectValue::Bool(_) => None,
        SNMPBotResultEntryObjectValue::Empty => None,
        SNMPBotResultEntryObjectValue::Other(_) => None,
    }
}

fn obj_str(objects: &HashMap<String, SNMPBotResultEntryObjectValue>, key: &str) -> Option<String> {
    match objects.get(key) {
        Some(SNMPBotResultEntryObjectValue::Str(v)) => Some(v.clone()),
        _ => None,
    }
}

// binascii.unhexlify(x).decode('ascii', errors='ignore') equivalent: strip
// whitespace, hex-decode, keep only ASCII bytes.
fn try_unhex_ascii(s: &str) -> Option<String> {
    let hex: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if hex.len() % 2 != 0 || hex.is_empty() {
        return None;
    }
    let mut out = String::new();
    let mut i = 0;
    while i < hex.len() {
        match u8::from_str_radix(&hex[i..i + 2], 16) {
            Ok(b) => {
                if b.is_ascii() {
                    out.push(b as char);
                }
            },
            Err(_) => return None,
        }
        i += 2;
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Per-device SNMP data source (port of lib/SNMPDataSource.py)
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq)]
enum DeviceBug {
    LldpMacaddressDuplicate,
    LldpNoAssociationToInterface,
    LldpMacaddressCannotAssociate,
}

#[derive(Clone)]
struct LldpNeighbor {
    rem_sys_name: String,
    rem_chassis_id: String,
    rem_port_id: String,
    rem_port_id_subtype: String,
}

#[derive(Clone)]
struct CdpNeighbor {
    device_id: String,
    device_port: String,
}

#[derive(Clone, Default)]
struct DiscoveredIface {
    ifindex: i64,
    name: Option<String>,         // IF-MIB::ifName (ifXTable)
    alias: Option<String>,        // IF-MIB::ifAlias (ifXTable)
    descr: Option<String>,        // IF-MIB::ifDescr (ifTable)
    iftype: Option<String>,       // IF-MIB::ifType (ifTable)
    phys_address: Option<String>, // IF-MIB::ifPhysAddress (ifTable)
    lldp: Option<LldpNeighbor>,
    cdp: Option<CdpNeighbor>,
}

impl DiscoveredIface {
    fn if_name(&self) -> String {
        // _ensure_interface_sanity ran before this is ever used for output.
        self.name.clone().unwrap_or_default()
    }
}

struct DetectedDevice {
    fqdn: String,
    community: String,
    interfaces: HashMap<i64, DiscoveredIface>,
    anything_to_interface: HashMap<String, i64>,
    lldp_index_to_interface: HashMap<i64, i64>,
    lldp_local_mapping: HashMap<String, i64>,
    device_bugs: Vec<DeviceBug>,
    bridge_address: Option<String>,   // BRIDGE-MIB::dot1dBaseBridgeAddress (raw, space-separated)
    lldp_loc_chassis_id: Option<String>, // colonized at store time, like Python
    sys_descr: Option<String>,
    device_types: Vec<String>,
    software_versions: Vec<String>,
    polling_valid: bool,
}

impl DetectedDevice {
    fn new(fqdn: &str, community: &str) -> DetectedDevice {
        DetectedDevice {
            fqdn: fqdn.to_string(),
            community: community.to_string(),
            interfaces: HashMap::new(),
            anything_to_interface: HashMap::new(),
            lldp_index_to_interface: HashMap::new(),
            lldp_local_mapping: HashMap::new(),
            device_bugs: Vec::new(),
            bridge_address: None,
            lldp_loc_chassis_id: None,
            sys_descr: None,
            device_types: Vec::new(),
            software_versions: Vec::new(),
            polling_valid: true,
        }
    }

    fn has_bug(&self, bug: DeviceBug) -> bool {
        self.device_bugs.contains(&bug)
    }

    fn add_bug(&mut self, bug: DeviceBug) {
        if !self.has_bug(bug.clone()) {
            self.device_bugs.push(bug);
        }
    }

    fn get_chassis_id(&self) -> Option<String> {
        if let Some(ref bridge_address) = self.bridge_address {
            return Some(bridge_address.replace(" ", ":"));
        }
        if let Some(ref chassis_id) = self.lldp_loc_chassis_id {
            return Some(chassis_id.clone());
        }
        println!("[discovery] [{}] could not derive chassis id when requested!", self.fqdn);
        None
    }

    fn device_type(&self) -> String {
        if self.device_types.is_empty() { "UNKNOWN".to_string() } else { self.device_types.join(",") }
    }

    fn software_version(&self) -> String {
        if self.software_versions.is_empty() { "UNKNOWN".to_string() } else { self.software_versions.join(",") }
    }

    fn os_info(&self) -> String {
        self.sys_descr.clone().unwrap_or_else(|| "UNKNOWN".to_string())
    }

    // --- collection ---

    fn collect(&mut self, client: &reqwest::blocking::Client, snmpbot_url: &str) -> Result<(), String> {
        self.bridge_address = fetch_object(client, snmpbot_url, &self.fqdn, &self.community, "BRIDGE-MIB::dot1dBaseBridgeAddress");
        self.sys_descr = fetch_object(client, snmpbot_url, &self.fqdn, &self.community, "SNMPv2-MIB::sysDescr");
        self.get_ifmibs(client, snmpbot_url)?;
        self.build_anything_to_interface();
        self.get_lldp_tables(client, snmpbot_url);
        self.get_cdp_tables(client, snmpbot_url);
        self.get_entmib_tables(client, snmpbot_url);
        self.ensure_interface_sanity();
        Ok(())
    }

    fn get_ifmibs(&mut self, client: &reqwest::blocking::Client, snmpbot_url: &str) -> Result<(), String> {
        // ifXTable first, then ifTable: matches the Python handler-dict order.
        // Both are critical tables — any failure invalidates the whole result.
        for table in ["IF-MIB::ifXTable", "IF-MIB::ifTable"].iter() {
            match fetch_table(client, snmpbot_url, &self.fqdn, &self.community, table) {
                Ok(response) => {
                    for entry in response.entries.iter() {
                        self.merge_ifmib_entry(entry);
                    }
                },
                Err(e) => {
                    println!("[discovery] [{}] critical table {} failed: {}", self.fqdn, table, e);
                    self.polling_valid = false;
                    return Err(format!("critical table {} failed: {}", table, e));
                }
            }
        }
        Ok(())
    }

    fn merge_ifmib_entry(&mut self, entry: &SNMPBotResultEntry) {
        let ifindex = match entry.index.get("IF-MIB::ifIndex") {
            Some(v) => *v,
            None => return,
        };
        let iface = self.interfaces.entry(ifindex).or_insert_with(|| {
            let mut i = DiscoveredIface::default();
            i.ifindex = ifindex;
            i
        });
        if let Some(v) = obj_str(&entry.objects, "IF-MIB::ifName") { iface.name = Some(v); }
        if let Some(v) = obj_str(&entry.objects, "IF-MIB::ifAlias") { iface.alias = Some(v); }
        if let Some(v) = obj_str(&entry.objects, "IF-MIB::ifDescr") { iface.descr = Some(v); }
        if let Some(v) = obj_str(&entry.objects, "IF-MIB::ifType") { iface.iftype = Some(v); }
        if let Some(v) = obj_str(&entry.objects, "IF-MIB::ifPhysAddress") { iface.phys_address = Some(v); }
    }

    fn is_valid_mac_keyed_interface(&self, iface: &DiscoveredIface) -> bool {
        match iface.iftype {
            None => true, // no ifType: presume OK
            Some(ref t) => t == "ethernetCsmacd",
        }
    }

    fn build_anything_to_interface(&mut self) {
        // Sorted for determinism where Python relied on dict insertion order.
        let mut ifindexes: Vec<i64> = self.interfaces.keys().cloned().collect();
        ifindexes.sort();
        let mut mapping: HashMap<String, i64> = HashMap::new();
        for ifindex in ifindexes.iter() {
            let iface = &self.interfaces[ifindex];
            let mut candidates: Vec<(Option<&String>, bool)> = vec![
                (iface.alias.as_ref(), false),
                (iface.name.as_ref(), false),
                (iface.descr.as_ref(), false),
            ];
            if self.is_valid_mac_keyed_interface(iface) {
                candidates.push((iface.phys_address.as_ref(), true));
            }
            for (candidate, is_mac) in candidates.into_iter() {
                let value = match candidate {
                    Some(v) => v.trim().to_string(),
                    None => continue,
                };
                if value.is_empty() {
                    continue;
                }
                if is_mac {
                    // First interface owning a MAC wins; register both colon-
                    // and space-separated spellings.
                    let spaced = value.replace(":", " ");
                    if mapping.contains_key(&value) || mapping.contains_key(&spaced) {
                        continue;
                    }
                    mapping.insert(value, *ifindex);
                    mapping.insert(spaced, *ifindex);
                } else {
                    mapping.insert(value, *ifindex);
                }
            }
        }
        self.anything_to_interface = mapping;
    }

    fn get_lldp_tables(&mut self, client: &reqwest::blocking::Client, snmpbot_url: &str) {
        self.lldp_loc_chassis_id = fetch_object(client, snmpbot_url, &self.fqdn, &self.community, "LLDP-MIB::lldpLocChassisId")
            .map(|v| v.replace(" ", ":"));
        match fetch_table(client, snmpbot_url, &self.fqdn, &self.community, "LLDP-MIB::lldpLocPortTable") {
            Ok(response) => self.handle_lldp_loc_port_table(&response.entries),
            Err(e) => println!("[discovery] [{}] table LLDP-MIB::lldpLocPortTable failed: {}", self.fqdn, e),
        }
        match fetch_table(client, snmpbot_url, &self.fqdn, &self.community, "LLDP-MIB::lldpRemTable") {
            Ok(response) => self.handle_lldp_rem_table(&response.entries),
            Err(e) => println!("[discovery] [{}] table LLDP-MIB::lldpRemTable failed: {}", self.fqdn, e),
        }
    }

    fn handle_lldp_loc_port_table(&mut self, entries: &Vec<SNMPBotResultEntry>) {
        // Pass 1: detect vendor bug where macAddress lldpLocPortIds are non-unique.
        let mut mac_uniqueness_test: HashSet<String> = HashSet::new();
        for entry in entries.iter() {
            let porttype = obj_str(&entry.objects, "LLDP-MIB::lldpLocPortIdSubtype").unwrap_or_default();
            let portid = obj_str(&entry.objects, "LLDP-MIB::lldpLocPortId").unwrap_or_default();
            if porttype == "macAddress" {
                if self.has_bug(DeviceBug::LldpMacaddressDuplicate) {
                    continue;
                }
                if mac_uniqueness_test.contains(&portid) {
                    println!("[discovery] [{}] VENDOR-BUG: LLDP macAddress as lldpLocPortId is non-unique!", self.fqdn);
                    self.device_bugs.push(DeviceBug::LldpMacaddressDuplicate);
                } else {
                    mac_uniqueness_test.insert(portid);
                }
            }
        }
        // Pass 2: build the lldpLocPortNum -> interface mapping.
        for entry in entries.iter() {
            let lldp_index = match entry.index.get("LLDP-MIB::lldpLocPortNum") {
                Some(v) => *v,
                None => continue,
            };
            let porttype = obj_str(&entry.objects, "LLDP-MIB::lldpLocPortIdSubtype").unwrap_or_default();
            let portid = obj_str(&entry.objects, "LLDP-MIB::lldpLocPortId").unwrap_or_default();
            if porttype == "macAddress" {
                self.build_lldp_mapping_from_mac_address(lldp_index, &portid);
            } else {
                if let Some(ifindex) = self.anything_to_interface.get(&portid) {
                    self.lldp_index_to_interface.insert(lldp_index, *ifindex);
                }
                let portdesc = obj_str(&entry.objects, "LLDP-MIB::lldpLocPortDesc").unwrap_or_default();
                if !portdesc.is_empty() && portdesc.chars().all(|c| c.is_ascii_digit()) && porttype == "local" {
                    if let Ok(portdesc_ifindex) = portdesc.parse::<i64>() {
                        if self.interfaces.contains_key(&portdesc_ifindex) {
                            self.lldp_local_mapping.insert(portid.clone(), portdesc_ifindex);
                        }
                    }
                }
                if porttype == "local" || porttype == "interfaceName" {
                    self.lldp_try_map_id_using_anything(lldp_index, &portid);
                }
            }
        }
    }

    fn build_lldp_mapping_from_mac_address(&mut self, lldp_index: i64, portid: &String) {
        if self.has_bug(DeviceBug::LldpMacaddressDuplicate) {
            // perhaps lldp-id = ifindex then lol
            if self.interfaces.contains_key(&lldp_index) {
                self.lldp_index_to_interface.insert(lldp_index, lldp_index);
            }
        } else if let Some(ifindex) = self.anything_to_interface.get(portid) {
            self.lldp_index_to_interface.insert(lldp_index, *ifindex);
        } else {
            if !self.has_bug(DeviceBug::LldpMacaddressCannotAssociate) {
                self.add_bug(DeviceBug::LldpMacaddressCannotAssociate);
                println!("[discovery] [{}] VENDOR-BUG: cannot associate LLDP ID (MAC) to interface MAC!", self.fqdn);
            }
            // perhaps lldp-id = ifindex then lol
            if self.interfaces.contains_key(&lldp_index) {
                self.lldp_index_to_interface.insert(lldp_index, lldp_index);
            }
        }
    }

    fn lldp_try_map_id_using_anything(&mut self, lldp_index: i64, portid: &str) {
        let decoded_port = match try_unhex_ascii(portid) {
            Some(v) => v,
            None => return,
        };
        if !decoded_port.is_empty() && decoded_port.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(decoded_port_num) = decoded_port.parse::<i64>() {
                if self.interfaces.contains_key(&decoded_port_num) {
                    self.lldp_index_to_interface.insert(lldp_index, decoded_port_num);
                    return;
                }
            }
        }
        if let Some(ifindex) = self.anything_to_interface.get(&decoded_port) {
            self.lldp_index_to_interface.insert(lldp_index, *ifindex);
        }
        if decoded_port.starts_with("Eth") {
            let test_cisco_nexus_quirk = decoded_port.replace("Eth", "Ethernet");
            if let Some(ifindex) = self.anything_to_interface.get(&test_cisco_nexus_quirk).cloned() {
                if !self.has_bug(DeviceBug::LldpNoAssociationToInterface) {
                    self.add_bug(DeviceBug::LldpNoAssociationToInterface);
                    println!("[discovery] [{}] VENDOR-BUG: LLDP interface cannot be associated to real interface without guesswork!", self.fqdn);
                }
                self.lldp_index_to_interface.insert(lldp_index, ifindex);
            }
        }
    }

    fn handle_lldp_rem_table(&mut self, entries: &Vec<SNMPBotResultEntry>) {
        for entry in entries.iter() {
            let lldp_index = match entry.index.get("LLDP-MIB::lldpRemLocalPortNum") {
                Some(v) => *v,
                None => continue,
            };
            let mut target_ifindex = match self.lldp_index_to_interface.get(&lldp_index) {
                Some(v) => *v,
                None => continue,
            };
            // `.0` subinterface aliasing: report the neighbor on the parent.
            if let Some(iface) = self.interfaces.get(&target_ifindex) {
                if let Some(ref name) = iface.name {
                    if name.ends_with(".0") {
                        let parent = &name[..name.len() - 2];
                        if let Some(parent_ifindex) = self.anything_to_interface.get(parent) {
                            target_ifindex = *parent_ifindex;
                        }
                    }
                }
            }
            let rem_sys_name = obj_str(&entry.objects, "LLDP-MIB::lldpRemSysName").unwrap_or_default();
            if rem_sys_name.trim().is_empty() {
                continue;
            }
            let neighbor = LldpNeighbor {
                rem_sys_name: rem_sys_name,
                rem_chassis_id: obj_str(&entry.objects, "LLDP-MIB::lldpRemChassisId").unwrap_or_default(),
                rem_port_id: obj_str(&entry.objects, "LLDP-MIB::lldpRemPortId").unwrap_or_default(),
                rem_port_id_subtype: obj_str(&entry.objects, "LLDP-MIB::lldpRemPortIdSubtype").unwrap_or_default(),
            };
            if let Some(iface) = self.interfaces.get_mut(&target_ifindex) {
                if iface.lldp.is_some() {
                    println!("[discovery] [{}] duplicate lldp index, only first entry is used", self.fqdn);
                }
                // NB: matches the Python behavior, which (despite its warning
                // message) lets the last entry win.
                iface.lldp = Some(neighbor);
            }
        }
    }

    fn get_cdp_tables(&mut self, client: &reqwest::blocking::Client, snmpbot_url: &str) {
        match fetch_table(client, snmpbot_url, &self.fqdn, &self.community, "CISCO-CDP-MIB::cdpCacheTable") {
            Ok(response) => {
                for entry in response.entries.iter() {
                    let local_ifindex = match entry.index.get("CISCO-CDP-MIB::cdpCacheIfIndex") {
                        Some(v) => *v,
                        None => continue,
                    };
                    let neighbor = CdpNeighbor {
                        device_id: obj_str(&entry.objects, "CISCO-CDP-MIB::cdpCacheDeviceId").unwrap_or_default(),
                        device_port: obj_str(&entry.objects, "CISCO-CDP-MIB::cdpCacheDevicePort").unwrap_or_default(),
                    };
                    if let Some(iface) = self.interfaces.get_mut(&local_ifindex) {
                        iface.cdp = Some(neighbor);
                    }
                }
            },
            Err(e) => println!("[discovery] [{}] table CISCO-CDP-MIB::cdpCacheTable failed: {}", self.fqdn, e),
        }
    }

    fn get_entmib_tables(&mut self, client: &reqwest::blocking::Client, snmpbot_url: &str) {
        let response = match fetch_table(client, snmpbot_url, &self.fqdn, &self.community, "ENTITY-MIB::entPhysicalTable") {
            Ok(r) => r,
            Err(e) => {
                println!("[discovery] [{}] table ENTITY-MIB::entPhysicalTable failed: {}", self.fqdn, e);
                return;
            }
        };
        for entry in response.entries.iter() {
            let class = obj_str(&entry.objects, "ENTITY-MIB::entPhysicalClass").unwrap_or_default();
            let descr = obj_str(&entry.objects, "ENTITY-MIB::entPhysicalDescr");
            let model = obj_str(&entry.objects, "ENTITY-MIB::entPhysicalModelName");
            let mut valid_item = false;
            if class == "chassis" {
                valid_item = true;
            }
            if class == "module" {
                if let Some(ref descr) = descr {
                    if descr.contains("Supervisor") {
                        valid_item = true;
                    }
                }
                if let (Some(ref model), Some(ref descr)) = (&model, &descr) {
                    if model.contains("SUP") && !descr.contains("Daughterboard") {
                        valid_item = true;
                    }
                }
            }
            if let Some(ref descr) = descr {
                if descr.contains("Wireless LAN Controller") {
                    valid_item = true;
                }
            }
            if !valid_item {
                continue;
            }
            if let Some(model) = model {
                let model = model.trim().to_string();
                if !model.is_empty() {
                    self.device_types.push(model);
                }
            }
            if let Some(swrev) = obj_str(&entry.objects, "ENTITY-MIB::entPhysicalSoftwareRev") {
                let swrev = swrev.trim().to_string();
                if !swrev.is_empty() {
                    self.software_versions.push(swrev);
                }
            }
        }
    }

    fn ensure_interface_sanity(&mut self) {
        for iface in self.interfaces.values_mut() {
            if iface.name.is_none() {
                iface.name = iface.descr.clone();
            }
            if iface.iftype.is_none() {
                iface.iftype = Some("other".to_string());
            }
        }
    }

    // --- lookups used during link resolution ---

    fn lookup_port_by_cdp_info(&self, cdp_cache_device_port: &str) -> Option<i64> {
        self.anything_to_interface.get(cdp_cache_device_port).cloned()
    }

    fn lookup_port_by_lldp_remote_info(&self, lldp_remote_port_id: &str, lldp_remote_port_id_subtype: &str) -> Option<i64> {
        if lldp_remote_port_id_subtype == "macAddress" && self.has_bug(DeviceBug::LldpMacaddressDuplicate) {
            println!("[discovery] [{}] suffering from LLDP-MACADDRESS-DUPLICATE bug, returning None for lookup by macaddr", self.fqdn);
            return None;
        }
        if lldp_remote_port_id_subtype == "local" {
            if let Some(ifindex) = self.lldp_local_mapping.get(lldp_remote_port_id) {
                return Some(*ifindex);
            }
        }
        if let Some(ifindex) = self.anything_to_interface.get(lldp_remote_port_id) {
            return Some(*ifindex);
        }
        if lldp_remote_port_id_subtype == "local" || lldp_remote_port_id_subtype == "interfaceName" {
            // sometimes this seems to be encoded as hexstr...
            if let Some(decoded_port) = try_unhex_ascii(lldp_remote_port_id) {
                if let Some(ifindex) = self.anything_to_interface.get(&decoded_port) {
                    return Some(*ifindex);
                }
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// DNS resolution (port of discover.py try_resolve)
// ---------------------------------------------------------------------------

fn resolves(fqdn_with_trailing_dot: &str) -> bool {
    use std::net::ToSocketAddrs;
    (fqdn_with_trailing_dot, 0u16)
        .to_socket_addrs()
        .map(|mut addrs| addrs.next().is_some())
        .unwrap_or(false)
}

fn try_resolve(device_name: &str, params: &RunParams) -> Option<String> {
    let mut device_name = device_name.to_string();
    if let Some(remapped) = params.remap.get(&device_name) {
        device_name = remapped.clone();
    }
    if device_name.trim().is_empty() {
        return None;
    }
    match device_name.split_once('.') {
        Some((hn, dn)) => {
            let fqdn = format!("{}.{}", hn, dn);
            if params.skip_dns || resolves(&format!("{}.", fqdn)) {
                Some(fqdn)
            } else {
                println!("[discovery] failed to resolve fqdn {}", device_name);
                None
            }
        },
        None => {
            if params.skip_dns {
                return params.dns_domains.first().map(|d| format!("{}.{}", device_name, d));
            }
            for search_domain in params.dns_domains.iter() {
                let fqdn = format!("{}.{}", device_name, search_domain);
                if resolves(&format!("{}.", fqdn)) {
                    return Some(fqdn);
                }
            }
            println!("[discovery] failed to resolve {} using any search domain", device_name);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Crawl (port of discover.py perform_discovery/discover_device)
// ---------------------------------------------------------------------------

struct CrawlState {
    detected: HashMap<String, DetectedDevice>,
    in_flight: HashSet<String>,
}

struct CrawlShared {
    params: RunParams,
    state: Mutex<CrawlState>,
    handles: Mutex<Vec<thread::JoinHandle<()>>>,
    pool: db::Pool,
    msgbus: Arc<Mutex<MessageBus>>,
    cache_controller: Arc<Mutex<CacheController>>,
}

fn start_device_discovery(shared: &Arc<CrawlShared>, device_fqdn: String) {
    {
        let mut state = match shared.state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        if state.in_flight.contains(&device_fqdn) || state.detected.contains_key(&device_fqdn) {
            return;
        }
        state.in_flight.insert(device_fqdn.clone());
    }
    let thread_shared = shared.clone();
    let handle = thread::spawn(move || {
        discover_device(thread_shared, device_fqdn);
    });
    if let Ok(mut handles) = shared.handles.lock() {
        handles.push(handle);
    }
}

fn discovered_device_payload(sds: &DetectedDevice) -> Option<models::json::DiscoveredDevice> {
    let (device_name, device_domain) = sds.fqdn.split_once('.')?;
    let mut interfaces: HashMap<String, models::json::DiscoveredInterface> = HashMap::new();
    for iface in sds.interfaces.values() {
        let name = iface.if_name();
        interfaces.insert(name.clone(), models::json::DiscoveredInterface {
            index: iface.ifindex as i32,
            interface_type: iface.iftype.clone().unwrap_or_else(|| "other".to_string()),
            display_name: None,
            name: name,
            alias: iface.alias.clone(),
            description: iface.descr.clone(),
        });
    }
    Some(models::json::DiscoveredDevice {
        name: device_name.to_string(),
        dns_domain: device_domain.to_string(),
        snmp_community: Some(sds.community.clone()),
        base_mac: sds.get_chassis_id(),
        os_info: Some(sds.os_info()),
        interfaces: interfaces,
        device_type: Some(sds.device_type()),
        software_version: Some(sds.software_version()),
    })
}

fn discover_device(shared: Arc<CrawlShared>, device_fqdn: String) {
    println!("[discovery] [{}] started polling device", device_fqdn);
    let client = match reqwest::blocking::Client::builder().timeout(time::Duration::from_secs(60)).build() {
        Ok(c) => c,
        Err(_) => return,
    };
    let mut sds = DetectedDevice::new(&device_fqdn, &shared.params.community);
    match sds.collect(&client, &shared.params.snmpbot_url) {
        Ok(_) => {},
        Err(e) => {
            println!("[discovery] [{}] failed to discover: {}", device_fqdn, e);
            return;
        }
    }
    if !sds.polling_valid {
        println!("[discovery] [{}] discarding invalid discovery result", device_fqdn);
        return;
    }

    // Gather resolvable, non-ignored neighbor fqdns before handing sds over.
    let mut tmp_discovered_neighbors: Vec<String> = Vec::new();
    for iface in sds.interfaces.values() {
        if let Some(ref lldp) = iface.lldp {
            if let Some(fqdn) = try_resolve(lldp.rem_sys_name.trim(), &shared.params) {
                if !tmp_discovered_neighbors.contains(&fqdn) && !shared.params.ignore.contains(&fqdn) {
                    tmp_discovered_neighbors.push(fqdn);
                }
            }
        }
        if let Some(ref cdp) = iface.cdp {
            if let Some(fqdn) = try_resolve(cdp.device_id.trim(), &shared.params) {
                if !tmp_discovered_neighbors.contains(&fqdn) && !shared.params.ignore.contains(&fqdn) {
                    tmp_discovered_neighbors.push(fqdn);
                }
            }
        }
    }

    let payload = discovered_device_payload(&sds);
    if let Ok(mut state) = shared.state.lock() {
        state.detected.insert(device_fqdn.clone(), sds);
    }
    for neighbor in tmp_discovered_neighbors.into_iter() {
        start_device_discovery(&shared, neighbor);
    }
    if let Some(payload) = payload {
        if let Ok(mut conn) = shared.pool.get() {
            utilities::discovery::ingest_device(&mut *conn, &shared.msgbus, &shared.cache_controller, &payload);
        }
    }
    println!("[discovery] [{}] finished polling device", device_fqdn);
}

// ---------------------------------------------------------------------------
// Link resolution (port of discover.py build_connections + lookup_*)
// ---------------------------------------------------------------------------

fn lookup_lldp_neighbor<'a>(detected: &'a HashMap<String, DetectedDevice>, descriptor: &LldpNeighbor, params: &RunParams) -> Option<&'a DetectedDevice> {
    let fqdn = try_resolve(&descriptor.rem_sys_name, params);
    match fqdn {
        Some(ref fqdn) if detected.contains_key(fqdn) => detected.get(fqdn),
        _ => {
            let colond_chassis_id = descriptor.rem_chassis_id.replace(" ", ":");
            for detected_device in detected.values() {
                if detected_device.get_chassis_id() == Some(colond_chassis_id.clone()) {
                    return Some(detected_device);
                }
            }
            None
        }
    }
}

fn lookup_lldp_neighbor_port(
    detected: &HashMap<String, DetectedDevice>,
    local_device_fqdn: &str,
    local_port_ifindex: i64,
    descriptor: &LldpNeighbor,
    lldp_neighbor: &DetectedDevice,
    params: &RunParams,
    is_reverse: bool,
) -> Option<i64> {
    // direct lookup, this is the most reliable one in terms of getting the correct result
    if let Some(remote_port) = lldp_neighbor.lookup_port_by_lldp_remote_info(&descriptor.rem_port_id, &descriptor.rem_port_id_subtype) {
        return Some(remote_port);
    }
    if !is_reverse {
        let mut num_refs = 0;
        let mut last_checked_interface: Option<i64> = None;
        for rev_interface in lldp_neighbor.interfaces.values() {
            if let Some(ref rev_descriptor) = rev_interface.lldp {
                let rev_lldp_neighbor = lookup_lldp_neighbor(detected, rev_descriptor, params);
                if let Some(rev_lldp_neighbor) = rev_lldp_neighbor {
                    if rev_lldp_neighbor.fqdn == local_device_fqdn {
                        num_refs += 1;
                        last_checked_interface = Some(rev_interface.ifindex);
                        let rev_lldp_neighbor_port = lookup_lldp_neighbor_port(
                            detected, &lldp_neighbor.fqdn, rev_interface.ifindex,
                            rev_descriptor, rev_lldp_neighbor, params, true,
                        );
                        if rev_lldp_neighbor_port == Some(local_port_ifindex) {
                            return Some(rev_interface.ifindex);
                        }
                    }
                }
            }
        }
        if num_refs == 1 {
            if let Some(last_checked_interface) = last_checked_interface {
                return Some(last_checked_interface);
            }
        }
    }
    let local_port_name = detected.get(local_device_fqdn)
        .and_then(|d| d.interfaces.get(&local_port_ifindex))
        .map(|i| i.if_name())
        .unwrap_or_default();
    println!("[discovery] [LLDP] giving up on {}:{} ({})", local_device_fqdn, local_port_name, lldp_neighbor.fqdn);
    None
}

fn lookup_cdp_neighbor<'a>(detected: &'a HashMap<String, DetectedDevice>, descriptor: &CdpNeighbor, params: &RunParams) -> Option<&'a DetectedDevice> {
    let fqdn = try_resolve(&descriptor.device_id, params)?;
    detected.get(&fqdn)
}

// fqdn -> ifindex -> Some((peer_fqdn, peer_ifindex))
type LinkMap = HashMap<String, HashMap<i64, (String, i64)>>;

fn build_connections(detected: &HashMap<String, DetectedDevice>, params: &RunParams) -> LinkMap {
    let mut links: LinkMap = HashMap::new();
    for (fqdn, device) in detected.iter() {
        for (ifindex, interface) in device.interfaces.iter() {
            if interface.lldp.is_none() && interface.cdp.is_none() {
                continue;
            }
            let mut cdp_link_candidate: Option<(String, i64)> = None;
            if let Some(ref cdp_neighbor_descriptor) = interface.cdp {
                if let Some(cdp_neighbor) = lookup_cdp_neighbor(detected, cdp_neighbor_descriptor, params) {
                    match cdp_neighbor.lookup_port_by_cdp_info(&cdp_neighbor_descriptor.device_port) {
                        Some(cdp_neighbor_port) => {
                            cdp_link_candidate = Some((cdp_neighbor.fqdn.clone(), cdp_neighbor_port));
                        },
                        None => {
                            println!("[discovery] [CDP] giving up on {}:{} ({})", fqdn, interface.if_name(), cdp_neighbor.fqdn);
                        }
                    }
                }
            }
            let mut lldp_link_candidate: Option<(String, i64)> = None;
            if let Some(ref lldp_neighbor_descriptor) = interface.lldp {
                if let Some(lldp_neighbor) = lookup_lldp_neighbor(detected, lldp_neighbor_descriptor, params) {
                    if let Some(lldp_neighbor_port) = lookup_lldp_neighbor_port(
                        detected, fqdn, *ifindex, lldp_neighbor_descriptor, lldp_neighbor, params, false,
                    ) {
                        lldp_link_candidate = Some((lldp_neighbor.fqdn.clone(), lldp_neighbor_port));
                    }
                }
            }
            let link = lldp_link_candidate.or(cdp_link_candidate);
            if let Some(link) = link {
                let peer_name = detected.get(&link.0)
                    .and_then(|d| d.interfaces.get(&link.1))
                    .map(|i| i.if_name())
                    .unwrap_or_default();
                println!("[discovery] LINK {}:{} -> {}:{}", fqdn, interface.if_name(), link.0, peer_name);
                links.entry(fqdn.clone()).or_insert_with(HashMap::new).insert(*ifindex, link);
            }
        }
    }
    links
}

fn link_info_payload(device: &DetectedDevice, links: &LinkMap, detected: &HashMap<String, DetectedDevice>, topology_stable: bool) -> models::json::LinkInfo {
    let device_links = links.get(&device.fqdn);
    let mut interfaces: HashMap<String, Option<models::json::LinkPeerInfo>> = HashMap::new();
    for (ifindex, interface) in device.interfaces.iter() {
        let link = device_links.and_then(|l| l.get(ifindex));
        match link {
            Some((peer_fqdn, peer_ifindex)) => {
                let peer_info = peer_fqdn.split_once('.').and_then(|(peer_name, peer_domain)| {
                    let peer_iface_name = detected.get(peer_fqdn)
                        .and_then(|d| d.interfaces.get(peer_ifindex))
                        .map(|i| i.if_name())?;
                    Some(models::json::LinkPeerInfo {
                        name: peer_name.to_string(),
                        dns_domain: peer_domain.to_string(),
                        interface: peer_iface_name,
                    })
                });
                interfaces.insert(interface.if_name(), peer_info);
            },
            None => {
                interfaces.insert(interface.if_name(), None);
            }
        }
    }
    models::json::LinkInfo {
        device_fqdn: device.fqdn.clone(),
        interfaces: interfaces,
        topology_stable: topology_stable,
    }
}

// ---------------------------------------------------------------------------
// One full discovery run
// ---------------------------------------------------------------------------

struct RunResult {
    devices_found: u64,
    links_found: u64,
    error: Option<String>,
}

fn perform_discovery_run(
    params: RunParams,
    pool: &db::Pool,
    msgbus: &Arc<Mutex<MessageBus>>,
    cache_controller: &Arc<Mutex<CacheController>>,
) -> RunResult {
    let root_device = match try_resolve(&params.root_device, &params) {
        Some(root_device) => root_device,
        None => {
            return RunResult {
                devices_found: 0,
                links_found: 0,
                error: Some(format!("failed to resolve root device {}", params.root_device)),
            };
        }
    };

    let shared = Arc::new(CrawlShared {
        params: params.clone(),
        state: Mutex::new(CrawlState { detected: HashMap::new(), in_flight: HashSet::new() }),
        handles: Mutex::new(Vec::new()),
        pool: pool.clone(),
        msgbus: msgbus.clone(),
        cache_controller: cache_controller.clone(),
    });

    start_device_discovery(&shared, root_device);
    // Join until quiescent; finished workers may have spawned new ones.
    loop {
        let handles: Vec<thread::JoinHandle<()>> = match shared.handles.lock() {
            Ok(mut h) => h.drain(..).collect(),
            Err(_) => break,
        };
        if handles.is_empty() {
            break;
        }
        for handle in handles {
            let _ = handle.join();
        }
    }

    let state = match shared.state.lock() {
        Ok(s) => s,
        Err(_) => {
            return RunResult { devices_found: 0, links_found: 0, error: Some("crawl state poisoned".to_string()) };
        }
    };
    let detected = &state.detected;
    let links = build_connections(detected, &params);
    let links_found: u64 = links.values().map(|m| m.len() as u64).sum();

    for device in detected.values() {
        let payload = link_info_payload(device, &links, detected, params.topology_stable);
        if let Ok(mut conn) = pool.get() {
            utilities::discovery::ingest_links(&mut *conn, cache_controller, &payload);
        }
    }

    RunResult {
        devices_found: detected.len() as u64,
        links_found: links_found,
        error: None,
    }
}

// ---------------------------------------------------------------------------
// Supervisor
// ---------------------------------------------------------------------------

fn next_run_params(snmpbot_url: &str, skip_dns: bool, control: &Arc<Mutex<DiscoveryControl>>) -> Option<RunParams> {
    let mut control = match control.lock() {
        Ok(c) => c,
        Err(_) => return None,
    };
    let now = utilities::tools::get_time();

    let mut overrides: Option<models::json::DiscoveryRunRequest> = None;
    let mut start = false;
    if control.trigger_requested {
        overrides = control.trigger_overrides.take();
        control.trigger_requested = false;
        start = true;
    } else if control.config.periodic_enabled {
        // Floor the interval so a bad config (0) cannot turn periodic
        // discovery into a busy loop against snmpbot and the network.
        let interval_secs = std::cmp::max(control.config.interval_secs, 10);
        let due = match control.status.last_finished {
            Some(last_finished) => now - last_finished >= interval_secs as f64,
            None => true,
        };
        if due {
            start = true;
        }
    }
    if !start {
        return None;
    }

    let root_device = overrides.as_ref().and_then(|o| o.root_device.clone()).or_else(|| control.config.root_device.clone());
    let community = overrides.as_ref().and_then(|o| o.community.clone()).or_else(|| control.config.community.clone());
    let (root_device, community) = match (root_device, community) {
        (Some(r), Some(c)) => (r, c),
        _ => {
            control.status.last_error = Some("discovery not configured: root device and community required".to_string());
            return None;
        }
    };

    control.status.running = true;
    control.status.last_started = Some(now);
    control.status.last_error = None;

    Some(RunParams {
        snmpbot_url: snmpbot_url.to_string(),
        root_device: root_device,
        community: community,
        dns_domains: overrides.as_ref().and_then(|o| o.dns_domains.clone()).unwrap_or_else(|| control.config.dns_domains.clone()),
        ignore: control.config.ignore.clone(),
        remap: control.config.remap.clone(),
        topology_stable: overrides.as_ref().and_then(|o| o.topology_stable).unwrap_or(control.config.topology_stable),
        skip_dns: skip_dns,
    })
}

pub fn run(
    snmpbot_url: String,
    control: Arc<Mutex<DiscoveryControl>>,
    msgbus: Arc<Mutex<MessageBus>>,
    cache_controller: Arc<Mutex<CacheController>>,
    running: Arc<atomic::AtomicBool>,
) {
    println!("[discovery] starting in-process engine (snmpbot={})", snmpbot_url);
    let pool = db::connect();
    let skip_dns = std::env::var("JASPY_DISCOVERY_SKIP_DNS").map(|v| v == "1" || v == "true").unwrap_or(false);

    while running.load(atomic::Ordering::Relaxed) {
        if let Some(params) = next_run_params(&snmpbot_url, skip_dns, &control) {
            println!("[discovery] starting run (root={}, stable={})", params.root_device, params.topology_stable);
            let result = perform_discovery_run(params, &pool, &msgbus, &cache_controller);
            if let Ok(mut control) = control.lock() {
                control.status.running = false;
                control.status.last_finished = Some(utilities::tools::get_time());
                control.status.devices_found = Some(result.devices_found);
                control.status.links_found = Some(result.links_found);
                control.status.last_error = result.error.clone();
            }
            match result.error {
                Some(error) => println!("[discovery] run failed: {}", error),
                None => println!("[discovery] run finished: {} devices, {} links", result.devices_found, result.links_found),
            }
        }
        thread::sleep(time::Duration::from_millis(1000));
    }
    println!("[discovery] engine stopped");
}
