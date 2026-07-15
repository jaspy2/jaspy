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

// --- in-process discovery engine control DTOs ---

// In-memory engine configuration; seeded from JASPY_DISCOVERY_* env vars at
// startup, mutable via PUT /dev/discovery/config (resets to env on restart).
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryConfig {
    pub root_device: Option<String>,
    pub community: Option<String>,
    pub dns_domains: Vec<String>,
    pub ignore: Vec<String>,
    pub remap: HashMap<String, String>,
    pub topology_stable: bool,
    pub periodic_enabled: bool,
    pub interval_secs: u64,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryStatus {
    pub running: bool,
    pub last_started: Option<f64>,
    pub last_finished: Option<f64>,
    pub devices_found: Option<u64>,
    pub devices_failed: Option<u64>,
    pub links_found: Option<u64>,
    pub last_error: Option<String>,
}

// Optional per-run overrides for POST /dev/discovery/run.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryRunRequest {
    pub root_device: Option<String>,
    pub community: Option<String>,
    pub dns_domains: Option<Vec<String>>,
    pub topology_stable: Option<bool>,
}

// --- /api/v1 DTOs (web admin UI) ---

// GET /api/v1/system: which features are enabled and how the process is
// wired, so an admin can see how the system is operating from the UI.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiSystemStatus {
    pub version: String,
    pub startup_time: f64,
    pub snmpbot_url: String,
    pub db_url: String,
    pub poller_enabled: bool,
    pub poll_loop_msecs: u64,
    pub pinger_enabled: bool,
    // "pinger" or "poller": where device up/down comes from.
    pub device_status_source: String,
    pub entitypoller_enabled: bool,
    pub entitypoller_interval_msecs: u64,
    pub entitypoller_sensors_enabled: bool,
    pub entitypoller_stp_enabled: bool,
    pub mqtt_enabled: bool,
    pub mqtt_broker: Option<String>,
    pub mqtt_connected: Option<bool>,
    pub discovery_periodic_enabled: bool,
    pub discovery_interval_secs: u64,
    pub weathermap_dir: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiSummary {
    pub version: String,
    pub state_id: i64,
    pub startup_time: f64,
    pub event_name: Option<String>,
    pub device_count: u64,
    pub devices_up: u64,
    pub devices_down: u64,
    pub devices_unknown: u64,
    pub discovery: DiscoveryStatus,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiDevice {
    pub id: i32,
    pub fqdn: String,
    pub name: String,
    pub dns_domain: String,
    pub snmp_community: Option<String>,
    pub base_mac: Option<String>,
    pub polling_enabled: Option<bool>,
    pub os_info: Option<String>,
    pub device_type: Option<String>,
    pub software_version: Option<String>,
    pub up: Option<bool>,
    pub seconds_since_last_poll: Option<u64>,
    pub interface_count: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiInterface {
    pub id: i32,
    pub index: i32,
    pub name: String,
    pub display_name: Option<String>,
    pub alias: Option<String>,
    pub description: Option<String>,
    pub interface_type: String,
    pub polling_enabled: Option<bool>,
    pub speed_override: Option<i32>,
    pub connected_to: Option<ApiInterfaceConnection>,
    pub up: Option<bool>,
    pub speed: Option<i32>,
}

// Link peer of an interface; structured so the UI can link to the device.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ApiInterfaceConnection {
    pub fqdn: String,
    pub interface: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiDeviceDetail {
    pub device: ApiDevice,
    pub interfaces: Vec<ApiInterface>,
}

// GET /api/v1/devices/<fqdn>/entity: latest entitypoller results for one
// device, converted from the in-memory EntityMetricsStore. Empty vectors mean
// "no data (yet)" — the store only fills after the first poll cycle.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiDeviceEntity {
    pub sensors: Vec<ApiEntitySensor>,
    pub stp: Vec<ApiStpPort>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiEntitySensor {
    pub sensor_id: i64,
    pub name: String,
    pub description: String,
    pub value: f64,
    // Raw ENTITY-SENSOR-MIB type ("celsius", "voltsDC", ...); unit rendering
    // is a UI concern.
    pub value_type: String,
    pub interface_name: Option<String>,
    pub interface_id: Option<i64>,
    pub timestamp: u64, // msecs
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiStpPort {
    pub vlan: i64,
    pub stp_port_id: i64,
    pub interface_name: Option<String>,
    pub interface_id: Option<i64>,
    pub role: String,   // "designated", ..., "unknown"
    pub state: String,  // "forwarding", ..., "unknown"
    pub enabled: Option<bool>,
    pub designated_cost: i64,
    pub path_cost: i64,
    pub priority: i64,
    pub forward_transitions: i64,
    pub timestamp: u64, // msecs
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiEvent {
    pub name: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiResetResult {
    pub devices_deleted: u64,
}

// Error body for non-2xx API responses; the web UI surfaces `error` verbatim.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub error: String,
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
    pub chaddr: String,
    pub option82: HashMap<String, String>,
}
