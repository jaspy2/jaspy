#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterfaceMonitorInterfaceReport {
    pub if_index: i32,
    pub up: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterfaceMonitorReport {
    pub device_fqdn : String,
    pub interfaces: Vec<InterfaceMonitorInterfaceReport>,
}