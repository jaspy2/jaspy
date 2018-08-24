#[derive(Serialize, Deserialize)]
pub struct InterfaceMonitorInterfaceReport {
    pub if_index: i32,
    pub in_octets: Option<u64>,
    pub out_octets: Option<u64>,
    pub in_packets: Option<u64>,
    pub out_packets: Option<u64>,
    pub in_errors: Option<u64>,
    pub out_errors: Option<u64>,
    pub up: Option<bool>,
    pub speed: Option<i32>,
}

#[derive(Serialize, Deserialize)]
pub struct InterfaceMonitorReport {
    pub device_fqdn: String,
    pub interfaces: Vec<InterfaceMonitorInterfaceReport>,
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