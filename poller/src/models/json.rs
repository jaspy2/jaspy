use std::collections::{HashMap};
use std;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterfaceMonitorInterfaceReport {
    pub if_index: i32,
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
}

impl InterfaceMonitorInterfaceReport {
    pub fn from_snmpbot_result_entry(if_index: &i32, objects: &HashMap<String, SNMPBotResultEntryObjectValue>) -> InterfaceMonitorInterfaceReport {
        return InterfaceMonitorInterfaceReport {
            if_index: *if_index,
            in_octets: try_get_u64(objects.get("IF-MIB::ifHCInOctets")),
            out_octets: try_get_u64(objects.get("IF-MIB::ifHCOutOctets")),
            in_unicast_packets: try_get_u64(objects.get("IF-MIB::ifHCInUcastPkts")),
            in_multicast_packets: try_get_u64(objects.get("IF-MIB::ifHCInMulticastPkts")),
            in_broadcast_packets: try_get_u64(objects.get("IF-MIB::ifHCInBroadcastPkts")),
            out_unicast_packets: try_get_u64(objects.get("IF-MIB::ifHCOutUcastPkts")),
            out_multicast_packets: try_get_u64(objects.get("IF-MIB::ifHCOutMulticastPkts")),
            out_broadcast_packets: try_get_u64(objects.get("IF-MIB::ifHCOutBroadcastPkts")),
            in_errors: try_get_u64(objects.get("IF-MIB::ifInErrors")),
            out_errors: try_get_u64(objects.get("IF-MIB::ifOutErrors")),
            out_discards: try_get_u64(objects.get("IF-MIB::ifOutDiscards")),
            up: try_get_updown_as_bool(objects.get("IF-MIB::ifOperStatus")),
            speed: try_get_i32(objects.get("IF-MIB::ifHighSpeed")),
        }
    }
}

fn try_get_u64(val: Option<&SNMPBotResultEntryObjectValue>) -> Option<u64> {
    if let Some(val) = val {
        if let SNMPBotResultEntryObjectValue::Uint64(val) = val {
            return Some(*val);
        } else {
            return None;
        }
    } else {
        return None;
    }
}

fn try_get_i32(val: Option<&SNMPBotResultEntryObjectValue>) -> Option<i32> {
    if let Some(val) = val {
        if let SNMPBotResultEntryObjectValue::Uint64(val) = val {
            if *val > std::i32::MAX as u64 {
                // TODO: log? bad overflow :C
                return None;
            }
            let val_i32 : i32 = *val as i32;
            return Some(val_i32);
        } else {
            return None;
        }
    } else {
        return None;
    }
}

fn try_get_updown_as_bool(val: Option<&SNMPBotResultEntryObjectValue>) -> Option<bool> {
    if let Some(val) = val {
        if let SNMPBotResultEntryObjectValue::Str(val) = val {
            if val.to_lowercase() == "up" {
                return Some(true);
            } else {
                return Some(false);
            }
        } else {
            return None;
        }
    } else {
        return None;
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterfaceMonitorReport {
    pub device_fqdn: String,
    pub interfaces: Vec<InterfaceMonitorInterfaceReport>,
}

impl InterfaceMonitorReport {
    pub fn new(fqdn: &String) -> InterfaceMonitorReport {
        return InterfaceMonitorReport { device_fqdn: fqdn.clone(), interfaces: Vec::new() };
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolledDeviceInfo {
    pub device_fqdn: String,
    pub snmp_community: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: i32,
    pub name: String,
    pub dns_domain: String,
    pub snmp_community: Option<String>,
    pub base_mac: Option<String>,
    pub polling_enabled: Option<bool>,
    pub os_info: Option<String>
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum SNMPBotResultEntryObjectValue {
    Uint64(u64),
    Float64(f64),
    Str(String),
    Empty,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SNMPBotResultEntry {
    pub host_i_d: String,
    pub index: HashMap<String, i64>,
    pub objects: HashMap<String, SNMPBotResultEntryObjectValue>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SNMPBotResponse {
    pub i_d: String,
    pub index_keys: Vec<String>,
    pub object_keys: Vec<String>,
    pub entries: Vec<SNMPBotResultEntry>,
}
