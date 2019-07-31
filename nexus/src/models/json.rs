use std::collections::HashMap;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStatus {
    pub fqdn: String,
    pub up: Option<bool>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInterfaceStatus {
    pub name: String,
    pub neighbors: bool,
    pub up: Option<bool>,
    pub speed: Option<i32>,
    pub interface_type: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredDevice {
    pub name: String,
    pub dns_domain: String,
    pub snmp_community: Option<String>,
    pub base_mac: Option<String>,
    pub os_info: Option<String>,
    pub interfaces : HashMap<String, DiscoveredInterface>,
    pub device_type: Option<String>,
    pub software_version: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredInterface {
    pub index: i32,
    pub interface_type: String,
    pub display_name: Option<String>,
    pub name: String,
    pub alias: Option<String>,
    pub description: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkPeerInfo {
    pub name : String,
    pub dns_domain : String,
    pub interface : String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkInfo {
    pub device_fqdn : String,
    pub interfaces : HashMap<String, Option<LinkPeerInfo>>,
    pub topology_stable : bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceMonitorInfo {
    pub fqdn : String,
    pub up : Option<bool>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceMonitorResponse {
    pub state_id : i64,
    pub devices : Vec<DeviceMonitorInfo>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceMonitorReport {
    pub fqdn : String,
    pub up : bool,
}

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

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterfaceMonitorReport {
    pub device_fqdn: String,
    pub interfaces: Vec<InterfaceMonitorInterfaceReport>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WeathermapDeviceInterfaceConnectedTo {
    pub fqdn: String,
    pub interface: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WeathermapDeviceInterface {
    pub name: String,
    pub if_index: i32,
    pub connected_to: Option<WeathermapDeviceInterfaceConnectedTo>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WeathermapDevice {
    pub fqdn: String,
    pub interfaces: HashMap<String, WeathermapDeviceInterface>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WeathermapBase {
    pub devices: HashMap<String, WeathermapDevice>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeathermapStateDeviceInterfaceState {
    pub state: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeathermapStateDevice {
    pub state: bool,
    pub interfaces: HashMap<String, WeathermapStateDeviceInterfaceState>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeathermapStateBase {
    pub devices: HashMap<String, WeathermapStateDevice>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeathermapPositionInfoUpdateDeviceInfo {
    pub device_fqdn: String,
    pub x: f64,
    pub y: f64,
    pub super_node: bool,
    pub expanded_by_default: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeathermapPositionInfoDeviceInfo {
    pub x: f64,
    pub y: f64,
    pub super_node: bool,
    pub expanded_by_default: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeathermapPositionInfoBase {
    pub devices: HashMap<String, WeathermapPositionInfoDeviceInfo>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientLocationInfo {
    pub yiaddr: String,
    pub option82: HashMap<String, String>,
}
