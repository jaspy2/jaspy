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
    // Physical media/form-factor from ENTITY-MIB (see collectors::entity_media).
    // Defaulted so older /dev payloads without the field still deserialize.
    #[serde(default)]
    pub media: Option<String>,
    // CDP neighbor (CISCO-CDP-MIB) reported on this port — remote device id and
    // port, kept even when the neighbor is off-fleet. Additive: defaulted so
    // older /dev payloads still deserialize.
    #[serde(default)]
    pub cdp_device_id: Option<String>,
    #[serde(default)]
    pub cdp_device_port: Option<String>,
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
    // When true, discover only `root_device` and do NOT crawl its neighbors.
    // Single-device runs execute on their own lane (see collectors::discovery),
    // so they neither block nor are blocked by the periodic/manual full crawl.
    pub single_device: Option<bool>,
}

// --- /api/v1 DTOs (web admin UI) ---

// GET /api/v1/system/perf: core hot-path performance counters (from
// utilities::perfstats) for the Maintenance page. Counters are lifetime totals;
// the UI diffs successive polls for live rates. Times are pre-divided to ms.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiPerfStats {
    pub device_polls: u64,
    pub poll_overruns: u64,
    pub poll_iter_mean_ms: f64,
    pub poll_iter_max_ms: f64,
    pub unresponsive_polls: u64,
    pub snmp_queries: u64,
    pub snmp_errors: u64,
    pub snmp_error_pct: f64,
    pub snmp_mean_ms: f64,
    pub snmp_max_ms: f64,
    pub snmp_timeouts: u64,
    pub snmp_session_opens: u64,
    pub snmp_inflight: u64,
    pub snmp_inflight_max: u64,
    pub imds_lock_wait_mean_ms: f64,
    pub imds_lock_wait_max_ms: f64,
    pub imds_report_mean_ms: f64,
    pub interfaces_reported: u64,
    pub metrics_scrapes: u64,
    pub metrics_build_max_ms: f64,
}

// GET /api/v1/system: which features are enabled and how the process is
// wired, so an admin can see how the system is operating from the UI.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiSystemStatus {
    pub version: String,
    pub startup_time: f64,
    pub snmpbot_url: String,
    // Live snmpbot reachability probe: true = responding, false = not
    // responding/timed out, null = not applicable (embedded mode, no snmpbot).
    // Additive: default-deserialized for older payloads.
    #[serde(default)]
    pub snmpbot_connected: Option<bool>,
    // SNMP back end: "snmpbot" (external HTTP service) or "embedded" (in-process snmp2).
    #[serde(default)]
    pub snmp_mode: String,
    #[serde(default)]
    pub snmp_mib_dir: Option<String>,
    #[serde(default)]
    pub snmp_mibs_loaded: Option<usize>,
    #[serde(default)]
    pub trap_receiver_enabled: bool,
    #[serde(default)]
    pub trap_bind_address: Option<String>,
    pub db_url: String,
    pub db_backend: String, // "postgresql" | "sqlite"
    pub db_connected: bool,
    // None when the database is unreachable (state unknown).
    pub db_migrations_pending: Option<bool>,
    pub poller_enabled: bool,
    pub poll_loop_msecs: u64,
    pub pinger_enabled: bool,
    // "pinger" or "poller": where device up/down comes from.
    pub device_status_source: String,
    pub entitypoller_enabled: bool,
    pub entitypoller_interval_msecs: u64,
    pub entitypoller_sensors_enabled: bool,
    pub entitypoller_stp_enabled: bool,
    pub vlanpoller_enabled: bool,
    pub vlanpoller_interval_msecs: u64,
    #[serde(default)]
    pub lagpoller_enabled: bool,
    #[serde(default)]
    pub lagpoller_interval_msecs: u64,
    pub mqtt_enabled: bool,
    pub mqtt_broker: Option<String>,
    pub mqtt_connected: Option<bool>,
    pub discovery_periodic_enabled: bool,
    pub discovery_interval_secs: u64,
    pub weathermap_dir: Option<String>,
    #[serde(default)]
    pub megaexcel_url: Option<String>,
}

// GET /api/v1/system/env: one JASPY_* environment variable and its (redacted)
// value, so an admin can see the effective startup configuration. Secret-ish
// values (SNMP communities, passwords, URL credentials) are masked server-side.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiEnvVar {
    pub name: String,
    pub value: String,
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
    // Worst per-interface health severity ("warn"/"bad") across this device, so
    // the device list can flag problem devices; null when all healthy. Additive.
    #[serde(default)]
    pub interface_health: Option<String>,
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
    // CDP-reported neighbor for a port whose far end is NOT a monitored device
    // (so connected_to is null). Shown as plain text, never a link. Additive.
    #[serde(default)]
    pub cdp_neighbor: Option<ApiCdpNeighbor>,
    pub up: Option<bool>,
    pub speed: Option<i32>,
    // Cumulative counters since the device's last counter reset (octets =
    // ifHCIn/OutOctets, errors/discards = ifInErrors/ifOutErrors/ifOutDiscards),
    // from IMDS. Additive: default-deserialized for older payloads.
    #[serde(default)]
    pub in_octets: Option<u64>,
    #[serde(default)]
    pub out_octets: Option<u64>,
    #[serde(default)]
    pub in_errors: Option<u64>,
    #[serde(default)]
    pub out_errors: Option<u64>,
    #[serde(default)]
    pub out_discards: Option<u64>,
    // VLAN membership from the in-memory vlanpoller store; null until the
    // first successful VLAN poll (or when the device does not expose the VLAN
    // MIBs). Additive fields: default-deserialized for older payloads.
    #[serde(default)]
    pub native_vlan: Option<i64>,
    #[serde(default)]
    pub tagged_vlans: Option<Vec<i64>>,
    // Name of the port-channel this interface is a member of (lagpoller
    // store); null for non-members. Additive: default-deserialized.
    #[serde(default)]
    pub port_channel: Option<String>,
    // Recent-history health signals (utilities::health); null when the
    // interface is healthy, so the UI shows nothing. Additive.
    #[serde(default)]
    pub health: Option<ApiInterfaceHealth>,
    // Physical media/form-factor from ENTITY-MIB (collectors::entity_media):
    // "copper", "sfp" (empty cage) or "sfp: <descr>" (populated). Live overlay
    // from the entitypoller when running, else the persisted discovery baseline.
    // null when unknown. Additive: default-deserialized.
    #[serde(default)]
    pub media: Option<String>,
    // Power-over-Ethernet state from the entitypoller (POWER-ETHERNET-MIB +
    // Cisco ext); null for non-PoE ports / devices. Additive: default-deserialized.
    #[serde(default)]
    pub poe: Option<ApiInterfacePoe>,
}

// Per-port PoE, surfaced only for PoE-capable ports. `status` is the
// POWER-ETHERNET-MIB detection status slug (deliveringPower/searching/
// disabled/fault/test/otherFault/other); watts are milliwatts from the Cisco
// extension and null on standards-only devices.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ApiInterfacePoe {
    pub status: String,
    pub admin_enabled: bool,
    pub class: Option<i64>,       // 0..=4
    pub power_mw: Option<i64>,    // real-time consumption
    pub allocated_mw: Option<i64>,
    pub max_drawn_mw: Option<i64>,
    pub priority: Option<String>, // critical/high/low
}

// Per-interface health summary surfaced only when a signal trips. The UI turns
// these numbers into human phrasing ("5,512 discards in last 5 minutes").
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiInterfaceHealth {
    pub severity: Option<String>, // "warn" | "bad"; null = healthy (no badge)
    pub flap_count: u32,
    pub last_flap_secs_ago: Option<u64>,
    pub in_errors: u64,
    pub out_errors: u64,
    pub discards: u64,
    pub speed_change_count: u32,
    // [from, to] Mbps of the most recent speed change (from may be null).
    pub last_speed_change: Option<(Option<i32>, i32)>,
    pub peak_utilization_pct: Option<f64>,
    pub high_utilization: bool,
    // Average throughput over the throughput window (bits/sec), rx/tx.
    pub rx_bps_avg: Option<f64>,
    pub tx_bps_avg: Option<f64>,
    pub stale: bool,
    pub counter_window_secs: u64,
    pub flap_window_secs: u64,
    pub util_window_secs: u64,
    pub throughput_window_secs: u64,
}

// Link peer of an interface; structured so the UI can link to the device.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ApiInterfaceConnection {
    pub fqdn: String,
    pub interface: String,
}

// A CDP-reported neighbor that is not a monitored jaspy device. Carries the
// raw CISCO-CDP-MIB device id and (optional) remote port, for display as text.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ApiCdpNeighbor {
    pub device_id: String,
    pub device_port: Option<String>,
}

// A VLAN known on the device (vtpVlanTable / dot1qVlanStaticTable), for
// resolving the ids in ApiInterface::{native,tagged}_vlans to names.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiVlan {
    pub id: i64,
    pub name: Option<String>,
}

// Per-VLAN bridge-level STP scalars (BRIDGE-MIB dot1dStp group) reported by
// one device: who it believes the root is and how the topology has churned.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ApiStpBridge {
    pub vlan: i64,
    pub root_priority: Option<i64>,
    pub root_mac: Option<String>, // lowercase colon form
    pub root_cost: Option<i64>,
    pub root_port: Option<i64>, // bridge port number
    pub root_port_interface_name: Option<String>,
    pub topology_changes: Option<i64>,
    pub time_since_topology_change_secs: Option<i64>,
    pub timestamp: u64,
}

// GET /api/v1/stp/<vlan>: the computed active spanning tree. Nodes are in
// DFS order (children sorted by fqdn) so a UI renders the indented tree by
// iterating linearly on depth.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ApiStpNode {
    pub fqdn: String,
    pub depth: i64,
    // Parent in the tree (None for roots and orphans) and the two link ends.
    pub parent: Option<String>,
    pub parent_interface: Option<String>,
    pub root_port_interface_name: Option<String>,
    pub root_port_state: Option<String>,
    pub path_cost: Option<i64>,
    pub reported: Option<ApiStpBridge>,
    // This device's reported root disagrees with the computed root's bridge MAC.
    pub root_mismatch: bool,
    // The triage context behind `root_mismatch` (both sides of the
    // disagreement, priorities, and whether the reported root is monitored).
    // Present only when `root_mismatch` is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_mismatch_detail: Option<ApiStpRootMismatchDetail>,
    // Has a root port but its upstream could not be resolved (missing
    // adjacency, unmonitored parent, or a cycle) — rendered as its own tree.
    pub orphan: bool,
}

// The two sides of an STP root-bridge disagreement, computed alongside the
// tree so the Issues view can explain who disagrees with whom and which claim
// STP would actually elect.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ApiStpRootMismatchDetail {
    // The root jaspy elected for this VLAN — the reference the mismatch is
    // measured against (the monitored bridge with no upstream root port).
    pub computed_root_fqdn: String,
    pub computed_root_hostname: String,
    pub computed_root_mac: Option<String>,
    pub computed_root_priority: Option<i64>,
    // What this node reports as the root bridge.
    pub reported_root_mac: Option<String>,
    pub reported_root_priority: Option<i64>,
    // The reported root's MAC belongs to a monitored (polled) device.
    pub reported_root_monitored: bool,
    // The reported root has a strictly better (lower) bridge ID than the
    // computed root — i.e. STP would elect it, so the computed root is the one
    // out of step. None when either priority is unknown.
    pub reported_root_superior: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ApiStpBlockedLink {
    pub fqdn: String,
    // Bridge port number: unique per (fqdn, vlan) even when the interface
    // name failed to resolve. Additive: default-deserialized for older payloads.
    #[serde(default)]
    pub stp_port_id: i64,
    pub interface_name: Option<String>,
    pub role: String, // "alternate" | "backUp"
    pub state: String,
    pub path_cost: i64,
    pub connected_to: Option<ApiInterfaceConnection>,
}

// One bridge that claims to be a VLAN's root (a monitored node with no root
// port). Part of the "multiple roots" evidence.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ApiStpRootClaim {
    pub fqdn: String,
    pub hostname: String,
    pub mac: Option<String>,
    pub priority: Option<i64>,
    // The lowest bridge ID among the claims — the root STP would elect if the
    // bridges converged.
    pub preferred: bool,
}

// The link joining two claimed roots, when they are directly connected, plus
// whether each end actually carries the VLAN (native or tagged). A link with
// the VLAN on only one end is an asymmetric trunk — the roots cannot merge.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ApiStpLinkEnds {
    pub a_fqdn: String,
    pub a_interface: String,
    pub a_has_vlan: bool,
    pub b_fqdn: String,
    pub b_interface: String,
    pub b_has_vlan: bool,
}

// Evidence for the "multiple-roots" flag: who claims root, which claim wins,
// and whether the claimants can actually reach each other (which distinguishes
// a real split brain from independently-monitored segments).
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ApiStpMultipleRootsDetail {
    pub roots: Vec<ApiStpRootClaim>,
    // Whether the claimed roots share a path in the discovered topology (a
    // direct link, for now). None when it can't be determined.
    pub adjacent: Option<bool>,
    // The link joining two of the roots, when directly connected.
    pub connecting_link: Option<ApiStpLinkEnds>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiStpTree {
    pub vlan: i64,
    pub roots: Vec<String>,
    pub nodes: Vec<ApiStpNode>,
    pub blocked_links: Vec<ApiStpBlockedLink>,
    // Structural anomalies: "no-root", "multiple-roots", "cycle",
    // "multiple-root-ports:<fqdn>".
    pub flags: Vec<String>,
    // Evidence behind a "multiple-roots" flag; present only when it fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiple_roots_detail: Option<ApiStpMultipleRootsDetail>,
}

// GET /api/v1/stp: which VLANs have STP data, for the page's VLAN selector.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiStpVlanSummary {
    pub vlan: i64,
    pub root_fqdn: Option<String>,
    pub node_count: i64,
    pub blocked_port_count: i64,
    pub topology_changes: Option<i64>,
    pub time_since_topology_change_secs: Option<i64>,
}

// GET /api/v1/vlans: network-wide VLAN inventory aggregated across every
// polled device, straight from the in-memory vlanpoller store.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiVlanDevice {
    pub fqdn: String,
    // This device's name for the VLAN (null when the device has no name row).
    pub name: Option<String>,
    pub native_ports: i64,
    pub tagged_ports: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiVlanSummary {
    pub id: i64,
    // Distinct names across devices, sorted; more than one entry means the
    // network disagrees about this VLAN's name.
    pub names: Vec<String>,
    pub devices: Vec<ApiVlanDevice>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiDeviceDetail {
    pub device: ApiDevice,
    pub interfaces: Vec<ApiInterface>,
    // Sorted by id; empty until the first successful VLAN poll. Additive:
    // default-deserialized for older payloads.
    #[serde(default)]
    pub vlans: Vec<ApiVlan>,
    // Port-channels from the in-memory lagpoller store; empty until the first
    // successful LAG poll. Additive: default-deserialized for older payloads.
    #[serde(default)]
    pub port_channels: Vec<ApiPortChannel>,
    // What the device fqdn resolves to at request time (v4 first); empty when
    // resolution fails. Additive: default-deserialized for older payloads.
    #[serde(default)]
    pub ip_addresses: Vec<String>,
    // Switch-wide PoE budget per PSE group (pethMainPseTable); empty for
    // non-PoE devices. Additive: default-deserialized for older payloads.
    #[serde(default)]
    pub poe_budget: Vec<ApiPoeBudget>,
}

// One PSE group's power budget for the device-wide PoE summary. Watts are
// whole-watt figures straight from pethMainPseTable; remaining/utilization are
// derived for the UI.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ApiPoeBudget {
    pub group: i64,
    pub total_w: i64,
    pub consumed_w: i64,
    pub remaining_w: i64,
    pub utilization_pct: i64,
    pub oper_on: bool,
}

// One link aggregate (Cisco port-channel / HP trk) with its member ports and
// the mismatch warnings computed against LACP state and the discovered
// topology (see lagpoller::port_channel_warnings for the codes).
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ApiPortChannel {
    pub ifindex: i64,
    pub name: Option<String>, // interface name of the aggregate, if known
    pub up: Option<bool>,
    pub protocol: String, // "lacp" | "pagp" | "static"
    pub partner_system_id: Option<String>,
    pub members: Vec<ApiPortChannelMember>,
    pub warnings: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ApiPortChannelMember {
    pub ifindex: i64,
    pub name: Option<String>,
    pub up: Option<bool>,
    pub connected_to: Option<ApiInterfaceConnection>,
    // IEEE 802.1AX LacpState bit names; empty for members that are configured
    // but not running LACP (mode "on", or link down).
    pub actor_state: Vec<String>,
    pub partner_state: Vec<String>,
    pub partner_port: Option<i64>,
    // synchronization + collecting + distributing all set.
    pub bundled: bool,
}

// GET /api/v1/devices/<fqdn>/entity: latest entitypoller results for one
// device, converted from the in-memory EntityMetricsStore. Empty vectors mean
// "no data (yet)" — the store only fills after the first poll cycle.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ApiDeviceEntity {
    pub sensors: Vec<ApiEntitySensor>,
    pub stp: Vec<ApiStpPort>,
    // Per-VLAN bridge scalars. Additive: default-deserialized for older payloads.
    #[serde(default)]
    pub stp_bridges: Vec<ApiStpBridge>,
}

#[derive(Serialize, Deserialize, Clone)]
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

#[derive(Serialize, Deserialize, Clone)]
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

impl InterfaceMonitorReport {
    // A single-interface up/down report, as produced by a linkUp/linkDown
    // trap. Shared by the snmptrapd `trap-handler` subcommand and the embedded
    // trap receiver so both ingest byte-identical reports.
    pub fn link_event(device_fqdn: &str, if_index: i32, up: bool) -> InterfaceMonitorReport {
        InterfaceMonitorReport {
            device_fqdn: device_fqdn.to_string(),
            interfaces: vec![InterfaceMonitorInterfaceReport {
                if_index,
                in_octets: None,
                out_octets: None,
                in_unicast_packets: None,
                in_multicast_packets: None,
                in_broadcast_packets: None,
                out_unicast_packets: None,
                out_multicast_packets: None,
                out_broadcast_packets: None,
                in_errors: None,
                out_errors: None,
                out_discards: None,
                up: Some(up),
                speed: None,
            }],
        }
    }
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

// One typed value in an issue's expanded detail. Tagged so the UI can render a
// MAC, a device link, an interface, a triage verdict or a hyperlink richly
// instead of as an opaque string. `Text` is the default every legacy signal
// still uses.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ApiIssueDetailValue {
    Text { text: String },
    // A bridge/host MAC. `monitored` is false when it belongs to no polled
    // device (an off-fleet bridge), which the UI flags.
    Mac { mac: String, monitored: bool },
    // A device the UI links to by fqdn.
    Device { fqdn: String, hostname: String },
    // An interface, optionally with its STP/oper state.
    Interface { name: String, state: Option<String> },
    // A short triage conclusion; `tone` ("good"|"bad"|"warn"|"neutral") drives
    // the badge colour.
    Verdict { text: String, tone: String },
    // A hyperlink (e.g. to the STP tree for the affected VLAN).
    Link { text: String, href: String },
    // A bridge claiming to be a VLAN root, with its bridge ID; `preferred`
    // marks the lowest ID (the one STP would actually elect). Rendered as a
    // device link followed by the priority/MAC.
    StpRoot { fqdn: String, hostname: String, mac: Option<String>, priority: Option<i64>, preferred: bool },
}

// A single label/value row in an issue's expanded detail.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ApiIssueDetail {
    pub label: String,
    pub value: ApiIssueDetailValue,
}

// GET /api/v1/issues: one derived fleet problem. Issues are computed on the fly
// from the in-memory stores (see utilities::issues); this DTO is enriched with
// tracker timestamps and, if present, the persisted acknowledgement.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ApiIssue {
    // Deterministic composite "<fqdn>|<kind>|<subject>" — stable across
    // re-derivation and restarts, so it keys the acknowledgement.
    pub issue_key: String,
    pub fqdn: String,
    pub hostname: String,
    pub kind: String,        // stable slug, e.g. "device-down", "iface-flapping"
    pub severity: String,    // "warn" | "bad"
    pub title: String,       // short human title
    pub description: String, // one-line human description
    // Human label for the affected sub-entity (interface name, "VLAN 10", "Po1"),
    // null for device-level issues.
    pub subject_label: Option<String>,
    // All known signal detail for the expanded view (ordered typed rows).
    pub detail: Vec<ApiIssueDetail>,
    pub first_seen: u64, // epoch ms of the current occurrence's onset
    pub last_seen: u64,  // epoch ms it was last observed active
    pub acknowledged: bool,
    pub acked_at: Option<i64>,
    pub acked_by: Option<String>,
    pub note: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ApiIssuesResponse {
    // Sorted by first_seen descending (most recent first). Both active and
    // acknowledged issues are included; `acknowledged` distinguishes them.
    pub issues: Vec<ApiIssue>,
}

// Request body for POST /api/v1/issues/ack and /unack.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiIssueAckRequest {
    pub issue_key: String,
    #[serde(default)]
    pub note: Option<String>,
}
