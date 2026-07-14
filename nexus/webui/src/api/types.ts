// DTOs mirroring the nexus /api/v1 JSON shapes (camelCase serde).

export interface DiscoveryStatus {
  running: boolean;
  lastStarted: number | null;
  lastFinished: number | null;
  devicesFound: number | null;
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
  connectedTo: string | null;
  up: boolean | null;
  speed: number | null;
}

export interface DeviceDetail {
  device: Device;
  interfaces: Interface[];
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
