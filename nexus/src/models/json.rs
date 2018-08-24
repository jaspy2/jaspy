use std::collections::HashMap;

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize)]
pub struct DiscoveredDevice {
    pub name: String,
    pub dnsDomain: String,
    pub snmpCommunity: Option<String>,
    pub baseMac: Option<String>,
    pub osInfo: Option<String>,
    pub interfaces : HashMap<String, DiscoveredInterface>,
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize)]
pub struct DiscoveredInterface {
    pub index: i32,
    pub interfaceType: String,
    pub displayName: Option<String>,
    pub name: String,
    pub alias: Option<String>,
    pub description: Option<String>,
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize)]
pub struct LinkPeerInfo {
    pub name : String,
    pub dnsDomain : String,
    pub interface : String,
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize)]
pub struct LinkInfo {
    pub deviceFqdn : String,
    pub interfaces : HashMap<String, Option<LinkPeerInfo>>,
    pub topologyStable : bool,
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize)]
pub struct DeviceMonitorInfo {
    pub fqdn : String,
    pub up : Option<bool>,
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize)]
pub struct DeviceMonitorResponse {
    pub devices : Vec<DeviceMonitorInfo>,
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize)]
pub struct DeviceMonitorReport {
    pub fqdn : String,
    pub up : bool,
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize)]
pub struct InterfaceMonitorInterfaceReport {
    pub ifIndex: i32,
    pub inOctets: Option<u64>,
    pub outOctets: Option<u64>,
    pub inPackets: Option<u64>,
    pub outPackets: Option<u64>,
    pub inErrors: Option<u64>,
    pub outErrors: Option<u64>,
    pub up: Option<bool>,
    pub speed: Option<i32>,
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize)]
pub struct InterfaceMonitorReport {
    pub deviceFqdn : String,
    pub interfaces: Vec<InterfaceMonitorInterfaceReport>,
}