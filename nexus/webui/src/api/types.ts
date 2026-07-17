// DTOs mirroring the nexus /api/v1 JSON shapes (camelCase serde).

// One line from a /api/v1/ws/logs/<topic> WebSocket (utilities/livelog.rs).
export interface LiveLogLine {
  ts: number;
  line: string;
}

// Msgbus event pushed on the "device:<fqdn>" WebSocket topic (models/events.rs).
// Only the payload matching eventType is present.
export interface LiveEvent {
  eventType: string;
  createTime: number;
  pingChange?: { fqdn: string; neighbors: string[]; oldState: boolean; newState: boolean };
  interfaceUpDown?: { fqdn: string; name: string; oldState: boolean; newState: boolean };
  interfaceSpeed?: { fqdn: string; name: string; oldState: number; newState: number };
}

export interface DiscoveryStatus {
  running: boolean;
  lastStarted: number | null;
  lastFinished: number | null;
  devicesFound: number | null;
  devicesFailed: number | null;
  linksFound: number | null;
  lastError: string | null;
}

export interface DiscoveryConfig {
  rootDevice: string | null;
  community: string | null;
  dnsDomains: string[];
  ignore: string[];
  remap: Record<string, string>;
  topologyStable: boolean;
  periodicEnabled: boolean;
  intervalSecs: number;
}

// GET /api/v1/system — effective feature configuration + connection state.
export interface PerfStats {
  devicePolls: number;
  pollOverruns: number;
  pollIterMeanMs: number;
  pollIterMaxMs: number;
  snmpQueries: number;
  snmpErrors: number;
  snmpErrorPct: number;
  snmpMeanMs: number;
  snmpMaxMs: number;
  snmpSessionOpens: number;
  snmpInflight: number;
  snmpInflightMax: number;
  imdsLockWaitMeanMs: number;
  imdsLockWaitMaxMs: number;
  imdsReportMeanMs: number;
  interfacesReported: number;
  metricsScrapes: number;
  metricsBuildMaxMs: number;
}

export interface SystemStatus {
  version: string;
  startupTime: number;
  snmpbotUrl: string;
  snmpbotConnected: boolean | null; // true=responding, false=down, null=embedded mode
  snmpMode: 'snmpbot' | 'embedded';
  dbUrl: string;
  dbBackend: string; // "postgresql" | "sqlite"
  dbConnected: boolean;
  dbMigrationsPending: boolean | null; // null = unknown (db unreachable)
  pollerEnabled: boolean;
  pollLoopMsecs: number;
  pingerEnabled: boolean;
  deviceStatusSource: 'pinger' | 'poller';
  entitypollerEnabled: boolean;
  entitypollerIntervalMsecs: number;
  entitypollerSensorsEnabled: boolean;
  entitypollerStpEnabled: boolean;
  vlanpollerEnabled: boolean;
  vlanpollerIntervalMsecs: number;
  mqttEnabled: boolean;
  mqttBroker: string | null;
  mqttConnected: boolean | null;
  discoveryPeriodicEnabled: boolean;
  discoveryIntervalSecs: number;
  weathermapDir: string | null;
}

export interface EnvVar {
  name: string;
  value: string; // redacted server-side (secrets masked)
}

export interface Summary {
  version: string;
  stateId: number;
  startupTime: number;
  eventName: string | null;
  deviceCount: number;
  devicesUp: number;
  devicesDown: number;
  devicesUnknown: number;
  discovery: DiscoveryStatus;
}

export interface Device {
  id: number;
  fqdn: string;
  name: string;
  dnsDomain: string;
  snmpCommunity: string | null;
  baseMac: string | null;
  pollingEnabled: boolean | null;
  osInfo: string | null;
  deviceType: string | null;
  softwareVersion: string | null;
  up: boolean | null;
  secondsSinceLastPoll: number | null;
  interfaceCount: number;
  // Worst per-interface health severity across the device ("warn"/"bad");
  // null when every interface is healthy.
  interfaceHealth: 'warn' | 'bad' | null;
}

// Per-interface health signals (utilities/health.rs), present only when a
// signal has tripped. The UI turns these numbers into human phrasing.
export interface InterfaceHealth {
  // null = healthy (no badge); the metric fields are always populated so the
  // expanded detail view can show the full picture for any interface.
  severity: 'warn' | 'bad' | null;
  flapCount: number;
  lastFlapSecsAgo: number | null;
  inErrors: number;
  outErrors: number;
  discards: number;
  speedChangeCount: number;
  lastSpeedChange: [number | null, number] | null; // [from, to] Mbps
  peakUtilizationPct: number | null;
  highUtilization: boolean;
  // Average throughput over throughputWindowSecs, in bits/sec (rx/tx).
  rxBpsAvg: number | null;
  txBpsAvg: number | null;
  stale: boolean;
  counterWindowSecs: number;
  flapWindowSecs: number;
  utilWindowSecs: number;
  throughputWindowSecs: number;
}

export interface InterfaceConnection {
  fqdn: string;
  interface: string;
}

export interface Interface {
  id: number;
  index: number;
  name: string;
  displayName: string | null;
  alias: string | null;
  description: string | null;
  interfaceType: string;
  pollingEnabled: boolean | null;
  speedOverride: number | null;
  connectedTo: InterfaceConnection | null;
  up: boolean | null;
  speed: number | null;
  // Cumulative counters since the device's last counter reset: octets (bytes)
  // and error/discard packet counts.
  inOctets: number | null;
  outOctets: number | null;
  inErrors: number | null;
  outErrors: number | null;
  outDiscards: number | null;
  // From the in-memory vlanpoller; null until the first successful VLAN poll
  // (or when the device does not expose the VLAN MIBs).
  nativeVlan: number | null;
  taggedVlans: number[] | null;
  // Name of the port-channel this interface is a member of (lagpoller);
  // null for non-members.
  portChannel: string | null;
  // Recent-history health signals + metrics; present for every polled
  // interface (health.severity is null when nothing is wrong), null only
  // before the first poll.
  health: InterfaceHealth | null;
}

// A VLAN known on the device, for resolving interface VLAN ids to names.
export interface Vlan {
  id: number;
  name: string | null;
}

// GET /api/v1/vlans — network-wide VLAN inventory from the vlanpoller.
export interface VlanDevice {
  fqdn: string;
  name: string | null; // this device's name for the VLAN
  nativePorts: number;
  taggedPorts: number;
}

export interface VlanSummary {
  id: number;
  names: string[]; // distinct names across devices; >1 = naming conflict
  devices: VlanDevice[];
}

// One link aggregate (Cisco port-channel / HP trk) from the lagpoller, with
// mismatch warnings computed against LACP state and the discovered topology.
export interface PortChannelMember {
  ifindex: number;
  name: string | null;
  up: boolean | null;
  connectedTo: InterfaceConnection | null;
  // IEEE 802.1AX LacpState bit names; empty when the member is configured
  // but not running LACP (mode "on", or link down).
  actorState: string[];
  partnerState: string[];
  partnerPort: number | null;
  bundled: boolean; // synchronization + collecting + distributing all set
}

export interface PortChannel {
  ifindex: number;
  name: string | null;
  up: boolean | null;
  protocol: string; // "lacp" | "pagp" | "static"
  partnerSystemId: string | null;
  members: PortChannelMember[];
  warnings: string[];
}

export interface DeviceDetail {
  device: Device;
  interfaces: Interface[];
  vlans: Vlan[];
  portChannels: PortChannel[];
  // What the fqdn resolves to at request time (v4 first); empty when
  // resolution fails.
  ipAddresses: string[];
}

// GET /api/v1/devices/<fqdn>/entity — latest entitypoller results. Empty
// arrays mean no data (yet): device without sensors/STP, poller disabled, or
// first poll cycle not finished.
export interface EntitySensor {
  sensorId: number;
  name: string;
  description: string;
  value: number;
  valueType: string; // raw ENTITY-SENSOR-MIB type, e.g. "celsius"
  interfaceName: string | null;
  interfaceId: number | null;
  timestamp: number; // msecs
}

export interface StpPort {
  vlan: number;
  stpPortId: number;
  interfaceName: string | null;
  interfaceId: number | null;
  role: string; // "designated", ..., "unknown"
  state: string; // "forwarding", ..., "unknown"
  enabled: boolean | null;
  designatedCost: number;
  pathCost: number;
  priority: number;
  forwardTransitions: number;
  timestamp: number; // msecs
}

// Per-VLAN bridge-level STP scalars reported by one device.
export interface StpBridge {
  vlan: number;
  rootPriority: number | null;
  rootMac: string | null;
  rootCost: number | null;
  rootPort: number | null;
  rootPortInterfaceName: string | null;
  topologyChanges: number | null;
  timeSinceTopologyChangeSecs: number | null;
  timestamp: number; // msecs
}

export interface DeviceEntity {
  sensors: EntitySensor[];
  stp: StpPort[];
  stpBridges: StpBridge[];
}

// GET /api/v1/stp — VLANs with STP data.
export interface StpVlanSummary {
  vlan: number;
  rootFqdn: string | null;
  nodeCount: number;
  blockedPortCount: number;
  topologyChanges: number | null;
  timeSinceTopologyChangeSecs: number | null;
}

// GET /api/v1/stp/<vlan> — the computed active spanning tree. Nodes are in
// DFS order so the indented tree renders by linear iteration on depth.
export interface StpNode {
  fqdn: string;
  depth: number;
  parent: string | null;
  parentInterface: string | null;
  rootPortInterfaceName: string | null;
  rootPortState: string | null;
  pathCost: number | null;
  reported: StpBridge | null;
  rootMismatch: boolean;
  orphan: boolean;
}

export interface StpBlockedLink {
  fqdn: string;
  // Bridge port number: unique per (fqdn, vlan) even when interfaceName is null.
  stpPortId: number;
  interfaceName: string | null;
  role: string; // "alternate" | "backUp"
  state: string;
  pathCost: number;
  connectedTo: InterfaceConnection | null;
}

export interface StpTree {
  vlan: number;
  roots: string[];
  nodes: StpNode[];
  blockedLinks: StpBlockedLink[];
  flags: string[];
}

// PUT/POST body for device create/update (nexus NewDevice).
export interface DeviceUpdate {
  name: string;
  dnsDomain: string;
  snmpCommunity: string | null;
  baseMac: string | null;
  pollingEnabled: boolean | null;
  osInfo: string | null;
  deviceType: string | null;
  softwareVersion: string | null;
}

export interface ClientLocation {
  id: number;
  deviceId: number;
  ipAddress: string;
  portInfo: string;
  hwAddress: string;
}

export interface EventInfo {
  name: string | null;
}

export interface ResetResult {
  devicesDeleted: number;
}
