use std::collections::HashMap;

#[derive(Serialize, Deserialize)]
pub struct DiscoveredDevice {
    pub name: String,
    pub dns_domain: String,
    pub snmp_community: Option<String>,
    pub base_mac: Option<String>,
    pub os_info: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct DiscoveredInterface {
    pub index: i32,
    pub interface_type: String,
    pub display_name: Option<String>,
    pub name: String,
    pub alias: Option<String>,
    pub description: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct DiscoveryInfo {
    pub device_info : DiscoveredDevice,
    pub interface_info : Vec<DiscoveredInterface>,
}

#[derive(Serialize, Deserialize)]
pub struct LinkPeerInfo {
    pub name : String,
    pub dns_domain : String,
    pub interface : String,
}

#[derive(Serialize, Deserialize)]
pub struct LinkInfo {
    pub device_fqdn : String,
    pub interfaces : HashMap<String, Option<LinkPeerInfo>>,
    pub topology_stable : bool,
}

#[derive(Serialize, Deserialize)]
pub struct DeviceMonitorInfo {
    pub fqdn : String,
    pub responsive : bool,
}

#[derive(Serialize, Deserialize)]
pub struct DeviceMonitorResponse {
    pub devices : Vec<DeviceMonitorInfo>,
}