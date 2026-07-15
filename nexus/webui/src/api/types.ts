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
export interface SystemStatus {
  version: string;
  startupTime: number;
  snmpbotUrl: string;
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
  // From the in-memory vlanpoller; null until the first successful VLAN poll
  // (or when the device does not expose the VLAN MIBs).
  nativeVlan: number | null;
  taggedVlans: number[] | null;
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

export interface DeviceDetail {
  device: Device;
  interfaces: Interface[];
  vlans: Vlan[];
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

export interface DeviceEntity {
  sensors: EntitySensor[];
  stp: StpPort[];
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
