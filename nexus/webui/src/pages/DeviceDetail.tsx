import { Fragment, useState } from 'react';
import { Link, useNavigate, useParams } from 'react-router-dom';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api } from '../api/client';
import type { Device, DeviceUpdate, Interface, LiveEvent } from '../api/types';
import { PollingBadge, StpStateBadge, UpBadge } from '../components/StatusBadge';
import useLiveSocket from '../hooks/useLiveSocket';

// ENTITY-SENSOR-MIB value types -> display units. Unknown types fall back to
// appending the raw type string so nothing is silently unitless.
const SENSOR_UNITS: Record<string, string> = {
  celsius: '°C',
  voltsDC: 'V DC',
  voltsAC: 'V AC',
  amperes: 'A',
  watts: 'W',
  hertz: 'Hz',
  percentRH: '%RH',
  rpm: 'RPM',
  dBm: 'dBm',
  truthvalue: '',
};

function formatSensorValue(value: number, valueType: string): string {
  const rounded = Math.round(value * 10) / 10;
  const unit = SENSOR_UNITS[valueType];
  if (unit === undefined) return `${rounded} ${valueType}`;
  return unit ? `${rounded} ${unit}` : `${rounded}`;
}

// Collapse a sorted VLAN list into ranges: [1, 10, 11, 12, 20] -> "1, 10-12, 20".
export function formatVlanRanges(vlans: number[]): string {
  const parts: string[] = [];
  for (let i = 0; i < vlans.length; ) {
    let j = i;
    while (j + 1 < vlans.length && vlans[j + 1] === vlans[j] + 1) j += 1;
    parts.push(j > i ? `${vlans[i]}-${vlans[j]}` : `${vlans[i]}`);
    i = j + 1;
  }
  return parts.join(', ');
}

// Expanded view of one interface's VLANs: native first, then every tagged
// VLAN, each with its id and (when the device reports one) its name.
function InterfaceVlanList({ iface, names }: { iface: Interface; names: Map<number, string | null> }) {
  const rows: { id: number; kind: 'native' | 'tagged' }[] = [];
  if (iface.nativeVlan !== null) rows.push({ id: iface.nativeVlan, kind: 'native' });
  for (const id of iface.taggedVlans ?? []) rows.push({ id, kind: 'tagged' });
  if (rows.length === 0) return <span className="muted">No VLAN data.</span>;
  return (
    <div className="vlan-list">
      {rows.map((row) => (
        <Fragment key={`${row.kind}-${row.id}`}>
          <span className={`badge ${row.kind === 'native' ? 'badge-ok' : 'badge-muted'}`}>{row.kind}</span>
          <span>{row.id}</span>
          <span className="muted">{names.get(row.id) ?? '—'}</span>
        </Fragment>
      ))}
    </div>
  );
}

// "· updated Ns ago" heading suffix from the newest row timestamp (msecs).
function UpdatedAgo({ timestamps }: { timestamps: number[] }) {
  if (timestamps.length === 0) return null;
  const secs = Math.max(0, Math.round((Date.now() - Math.max(...timestamps)) / 1000));
  return <span className="muted"> · updated {secs}s ago</span>;
}

function updateBody(device: Device, overrides: Partial<DeviceUpdate>): DeviceUpdate {
  return {
    name: device.name,
    dnsDomain: device.dnsDomain,
    snmpCommunity: device.snmpCommunity,
    baseMac: device.baseMac,
    pollingEnabled: device.pollingEnabled,
    osInfo: device.osInfo,
    deviceType: device.deviceType,
    softwareVersion: device.softwareVersion,
    ...overrides,
  };
}

export default function DeviceDetail() {
  const { fqdn = '' } = useParams();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [confirmDelete, setConfirmDelete] = useState(false);
  // Interface ids whose VLAN list is expanded (desktop row and mobile card).
  const [expandedVlans, setExpandedVlans] = useState<Set<number>>(new Set());
  const toggleVlans = (id: number) =>
    setExpandedVlans((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const detail = useQuery({
    queryKey: ['device', fqdn],
    queryFn: () => api.device(fqdn),
    // Fallback poll; live changes arrive over the device event socket below.
    refetchInterval: 10000,
  });

  // Entitypoller results (sensors + STP). Poll only — the entitypoller cycle
  // is slow (default 120s) and emits no msgbus events.
  const entity = useQuery({
    queryKey: ['device-entity', fqdn],
    queryFn: () => api.deviceEntity(fqdn),
    refetchInterval: 30000,
  });

  // Static feature config, to tell "entitypoller disabled" apart from "no
  // data for this device (yet)".
  const system = useQuery({
    queryKey: ['system'],
    queryFn: api.system,
    staleTime: 60000,
  });

  // The backend pushes this device's msgbus events (interface up/down/speed,
  // ping change, config changes) on a per-device topic; any of them means the
  // detail view is stale, so refetch immediately instead of waiting out the
  // poll interval.
  useLiveSocket(`device:${fqdn}`, {
    onMessage: (data) => {
      const event = data as LiveEvent;
      if (!event?.eventType) return;
      queryClient.invalidateQueries({ queryKey: ['device', fqdn] });
      if (event.eventType === 'pingChange') {
        // Device up/down also shows on the list and dashboard.
        queryClient.invalidateQueries({ queryKey: ['devices'] });
        queryClient.invalidateQueries({ queryKey: ['summary'] });
      }
    },
  });

  const invalidate = () => {
    queryClient.invalidateQueries({ queryKey: ['device', fqdn] });
    queryClient.invalidateQueries({ queryKey: ['devices'] });
    queryClient.invalidateQueries({ queryKey: ['summary'] });
  };

  const togglePolling = useMutation({
    mutationFn: (device: Device) =>
      api.updateDevice(fqdn, updateBody(device, { pollingEnabled: device.pollingEnabled === false ? null : false })),
    onSettled: invalidate,
  });

  const deleteDevice = useMutation({
    mutationFn: () => api.deleteDevice(fqdn),
    onSuccess: () => {
      invalidate();
      navigate('/devices');
    },
  });

  // 202 means "queued": the vlanpoller picks the device up on its next 1s
  // tick, so refetch shortly after instead of waiting out the poll interval.
  const pollVlans = useMutation({
    mutationFn: () => api.pollVlans(fqdn),
    onSuccess: () => {
      setTimeout(() => queryClient.invalidateQueries({ queryKey: ['device', fqdn] }), 3000);
    },
  });

  if (detail.isLoading) return <p>Loading…</p>;
  if (detail.isError || !detail.data) return <p className="error">Failed to load {fqdn}: {String(detail.error)}</p>;

  const { device, interfaces } = detail.data;
  const vlanNames = new Map((detail.data.vlans ?? []).map((v) => [v.id, v.name]));
  const sensors = entity.data?.sensors ?? [];
  const stp = entity.data?.stp ?? [];
  const entitypollerEnabled = system.data?.entitypollerEnabled === true;
  const vlanpollerEnabled = system.data?.vlanpollerEnabled === true;

  return (
    <>
      <h1>
        {device.fqdn} <UpBadge up={device.up} />
      </h1>
      <div className="panel">
        <div className="kv">
          <span>Type</span><span>{device.deviceType ?? '—'}</span>
          <span>Software</span><span>{device.softwareVersion ?? '—'}</span>
          <span>OS info</span><span className="wrap">{device.osInfo ?? '—'}</span>
          <span>Base MAC</span><span>{device.baseMac ?? '—'}</span>
          <span>SNMP community</span><span>{device.snmpCommunity ? '••••••' : '—'}</span>
          <span>Polling</span><span><PollingBadge enabled={device.pollingEnabled} /></span>
        </div>
        <div className="actions">
          <button onClick={() => togglePolling.mutate(device)} disabled={togglePolling.isPending}>
            {device.pollingEnabled === false ? 'Enable polling' : 'Disable polling'}
          </button>
          {vlanpollerEnabled && (
            <button onClick={() => pollVlans.mutate()} disabled={pollVlans.isPending}>
              Poll VLANs now
            </button>
          )}
          {!confirmDelete ? (
            <button className="danger" onClick={() => setConfirmDelete(true)}>
              Delete device…
            </button>
          ) : (
            <>
              <button className="danger" onClick={() => deleteDevice.mutate()} disabled={deleteDevice.isPending}>
                Confirm delete {device.fqdn}
              </button>
              <button onClick={() => setConfirmDelete(false)}>Cancel</button>
            </>
          )}
          {(togglePolling.isError || deleteDevice.isError || pollVlans.isError) && (
            <span className="error">{String(togglePolling.error ?? deleteDevice.error ?? pollVlans.error)}</span>
          )}
        </div>
      </div>

      <h2>Interfaces ({interfaces.length})</h2>
      <div className="item-list mobile-only">
        {interfaces.map((iface) => (
          <div key={iface.id} className="item-card">
            <span className="item-title">
              <span>{iface.displayName ?? iface.name}</span>
              <UpBadge up={iface.up} />
            </span>
            <span className="item-sub">
              {iface.speed !== null && <span>{iface.speed} Mb/s</span>}
              {(iface.nativeVlan !== null || iface.taggedVlans !== null) && (
                <button className="vlan-toggle" onClick={() => toggleVlans(iface.id)} aria-expanded={expandedVlans.has(iface.id)}>
                  VLAN {iface.nativeVlan ?? '—'}
                  {(iface.taggedVlans?.length ?? 0) > 0 && ` (+${iface.taggedVlans!.length} tagged)`}
                  {expandedVlans.has(iface.id) ? ' ▾' : ' ▸'}
                </button>
              )}
              {iface.alias && <span>{iface.alias}</span>}
            </span>
            {expandedVlans.has(iface.id) && (
              <InterfaceVlanList iface={iface} names={vlanNames} />
            )}
            {iface.connectedTo && (
              <span className="item-sub">
                <span>
                  →{' '}
                  <Link to={`/devices/${encodeURIComponent(iface.connectedTo.fqdn)}`}>
                    {iface.connectedTo.fqdn}
                  </Link>
                  :{iface.connectedTo.interface}
                </span>
              </span>
            )}
          </div>
        ))}
      </div>
      <div className="table-wrap desktop-only">
        <table>
          <thead>
            <tr>
              <th>Name</th>
              <th>State</th>
              <th>Speed</th>
              <th>VLAN</th>
              <th>Tagged VLANs</th>
              <th>Alias</th>
              <th>Type</th>
              <th>Connected to</th>
            </tr>
          </thead>
          <tbody>
            {interfaces.map((iface) => (
              <Fragment key={iface.id}>
                <tr>
                  <td>{iface.displayName ?? iface.name}</td>
                  <td><UpBadge up={iface.up} /></td>
                  <td>{iface.speed !== null ? `${iface.speed} Mb/s` : '—'}</td>
                  <td>
                    {iface.nativeVlan !== null || iface.taggedVlans !== null ? (
                      <button className="vlan-toggle" onClick={() => toggleVlans(iface.id)} aria-expanded={expandedVlans.has(iface.id)}>
                        {iface.nativeVlan ?? '—'}
                        {expandedVlans.has(iface.id) ? ' ▾' : ' ▸'}
                      </button>
                    ) : (
                      '—'
                    )}
                  </td>
                  <td className="wrap" title={iface.taggedVlans?.join(', ')}>
                    {(iface.taggedVlans?.length ?? 0) > 0 ? formatVlanRanges(iface.taggedVlans!) : '—'}
                  </td>
                  <td>{iface.alias ?? '—'}</td>
                  <td>{iface.interfaceType}</td>
                  <td>
                    {iface.connectedTo ? (
                      <>
                        <Link to={`/devices/${encodeURIComponent(iface.connectedTo.fqdn)}`}>
                          {iface.connectedTo.fqdn}
                        </Link>
                        :{iface.connectedTo.interface}
                      </>
                    ) : (
                      '—'
                    )}
                  </td>
                </tr>
                {expandedVlans.has(iface.id) && (
                  <tr className="vlan-detail-row">
                    <td colSpan={8}>
                      <InterfaceVlanList iface={iface} names={vlanNames} />
                    </td>
                  </tr>
                )}
              </Fragment>
            ))}
          </tbody>
        </table>
      </div>

      {sensors.length > 0 && (
        <>
          <h2>
            Sensors ({sensors.length})
            <UpdatedAgo timestamps={sensors.map((s) => s.timestamp)} />
          </h2>
          <div className="table-wrap">
            <table>
              <thead>
                <tr>
                  <th>Sensor</th>
                  <th>Value</th>
                  <th className="hide-mobile">Interface</th>
                  <th className="hide-mobile">Description</th>
                </tr>
              </thead>
              <tbody>
                {sensors.map((sensor) => (
                  <tr key={`${sensor.sensorId}-${sensor.name}-${sensor.valueType}`}>
                    <td className="wrap-mobile">{sensor.name || `sensor ${sensor.sensorId}`}</td>
                    <td>{formatSensorValue(sensor.value, sensor.valueType)}</td>
                    <td className="hide-mobile">{sensor.interfaceName ?? '—'}</td>
                    <td className="hide-mobile muted">{sensor.description || '—'}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}

      {stp.length > 0 && (
        <>
          <h2>
            STP ({stp.length})
            <UpdatedAgo timestamps={stp.map((p) => p.timestamp)} />
          </h2>
          <div className="table-wrap">
            <table>
              <thead>
                <tr>
                  <th>VLAN</th>
                  <th>Interface</th>
                  <th>Role</th>
                  <th>State</th>
                  <th className="hide-mobile">Enabled</th>
                  <th className="hide-mobile">Path cost</th>
                  <th className="hide-mobile">Designated cost</th>
                  <th className="hide-mobile">Priority</th>
                  <th className="hide-mobile">Fwd transitions</th>
                </tr>
              </thead>
              <tbody>
                {stp.map((port) => (
                  <tr key={`${port.vlan}-${port.stpPortId}`}>
                    <td><Link to={`/stp?vlan=${port.vlan}`} title="Open the STP tree for this VLAN">{port.vlan}</Link></td>
                    <td className="wrap-mobile">{port.interfaceName ?? `port ${port.stpPortId}`}</td>
                    <td>{port.role}</td>
                    <td><StpStateBadge state={port.state} /></td>
                    <td className="hide-mobile">{port.enabled === null ? '—' : port.enabled ? 'yes' : <span className="badge badge-warn">no</span>}</td>
                    <td className="hide-mobile">{port.pathCost}</td>
                    <td className="hide-mobile">{port.designatedCost}</td>
                    <td className="hide-mobile">{port.priority}</td>
                    <td className="hide-mobile">{port.forwardTransitions}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}

      {sensors.length === 0 && stp.length === 0 && entitypollerEnabled && (
        <p className="muted">No sensor/STP data for this device (yet).</p>
      )}
    </>
  );
}
