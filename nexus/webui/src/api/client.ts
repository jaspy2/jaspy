import type {
  ClientLocation,
  Device,
  DeviceDetail,
  DeviceEntity,
  DeviceUpdate,
  DiscoveryConfig,
  EnvVar,
  DiscoveryStatus,
  EventInfo,
  Issue,
  IssuesResponse,
  IssueAckRequest,
  ResetResult,
  StpTree,
  StpVlanSummary,
  PerfStats,
  Summary,
  SystemStatus,
  VlanSummary,
} from './types';

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    headers: { 'content-type': 'application/json' },
    ...init,
  });
  if (!response.ok) {
    // API errors carry a human-readable reason as {"error": "..."} — show it.
    let detail = '';
    try {
      const body = await response.json();
      if (body && typeof body.error === 'string') detail = ` — ${body.error}`;
    } catch {
      /* non-JSON error body */
    }
    throw new Error(`${init?.method ?? 'GET'} ${path}: ${response.status} ${response.statusText}${detail}`);
  }
  const text = await response.text();
  return (text ? JSON.parse(text) : undefined) as T;
}

export const api = {
  summary: () => request<Summary>('/api/v1/summary'),
  system: () => request<SystemStatus>('/api/v1/system'),
  systemPerf: () => request<PerfStats>('/api/v1/system/perf'),
  systemEnv: () => request<EnvVar[]>('/api/v1/system/env'),
  devices: () => request<Device[]>('/api/v1/devices'),
  device: (fqdn: string) => request<DeviceDetail>(`/api/v1/devices/${encodeURIComponent(fqdn)}`),
  deviceEntity: (fqdn: string) =>
    request<DeviceEntity>(`/api/v1/devices/${encodeURIComponent(fqdn)}/entity`),
  createDevice: (body: DeviceUpdate) =>
    request<Device>('/api/v1/devices', { method: 'POST', body: JSON.stringify(body) }),
  updateDevice: (fqdn: string, body: DeviceUpdate) =>
    request<Device>(`/api/v1/devices/${encodeURIComponent(fqdn)}`, {
      method: 'PUT',
      body: JSON.stringify(body),
    }),
  deleteDevice: (fqdn: string) =>
    request<Device>(`/api/v1/devices/${encodeURIComponent(fqdn)}`, { method: 'DELETE' }),
  clientLocations: () => request<ClientLocation[]>('/api/v1/clientlocations'),
  event: () => request<EventInfo>('/api/v1/event'),
  putEvent: (body: EventInfo) =>
    request<EventInfo>('/api/v1/event', { method: 'PUT', body: JSON.stringify(body) }),
  reset: () => request<ResetResult>('/api/v1/reset', { method: 'POST' }),
  discoveryStatus: () => request<DiscoveryStatus>('/api/v1/discovery/status'),
  discoveryConfig: () => request<DiscoveryConfig>('/api/v1/discovery/config'),
  putDiscoveryConfig: (body: DiscoveryConfig) =>
    request<DiscoveryConfig>('/api/v1/discovery/config', {
      method: 'PUT',
      body: JSON.stringify(body),
    }),
  runDiscovery: () =>
    request<DiscoveryStatus>('/api/v1/discovery/run', { method: 'POST', body: '{}' }),
  pollVlans: (fqdn: string) =>
    request<void>(`/api/v1/devices/${encodeURIComponent(fqdn)}/vlans/poll`, { method: 'POST' }),
  vlans: () => request<VlanSummary[]>('/api/v1/vlans'),
  stp: () => request<StpVlanSummary[]>('/api/v1/stp'),
  stpTree: (vlan: number) => request<StpTree>(`/api/v1/stp/${vlan}`),
  issues: () => request<IssuesResponse>('/api/v1/issues'),
  ackIssue: (body: IssueAckRequest) =>
    request<Issue>('/api/v1/issues/ack', { method: 'POST', body: JSON.stringify(body) }),
  unackIssue: (body: IssueAckRequest) =>
    request<void>('/api/v1/issues/unack', { method: 'POST', body: JSON.stringify(body) }),
};

// WebSocket endpoint for live log tailing (backlog replay + push). Relative to
// the current origin so it works via the vite dev proxy and in production.
export function liveLogSocketUrl(topic: string): string {
  const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
  return `${proto}//${window.location.host}/api/v1/ws/logs/${encodeURIComponent(topic)}`;
}

export function formatTimestamp(secs: number | null): string {
  if (secs === null) return '—';
  return new Date(secs * 1000).toLocaleString();
}

export function formatUptime(startupTime: number): string {
  const total = Math.max(0, Math.floor(Date.now() / 1000 - startupTime));
  const d = Math.floor(total / 86400);
  const h = Math.floor((total % 86400) / 3600);
  const m = Math.floor((total % 3600) / 60);
  if (d > 0) return `${d}d ${h}h ${m}m`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}
