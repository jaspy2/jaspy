import { useState } from 'react';
import { Link, useNavigate, useParams } from 'react-router-dom';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api } from '../api/client';
import type { Device, DeviceUpdate, LiveEvent } from '../api/types';
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

  if (detail.isLoading) return <p>Loading…</p>;
  if (detail.isError || !detail.data) return <p className="error">Failed to load {fqdn}: {String(detail.error)}</p>;

  const { device, interfaces } = detail.data;
  const sensors = entity.data?.sensors ?? [];
  const stp = entity.data?.stp ?? [];
  const entitypollerEnabled = system.data?.entitypollerEnabled === true;

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
          {(togglePolling.isError || deleteDevice.isError) && (
            <span className="error">{String(togglePolling.error ?? deleteDevice.error)}</span>
          )}
        </div>
      </div>

      <h2>Interfaces ({interfaces.length})</h2>
      <div className="table-wrap">
        <table>
          <thead>
            <tr>
              <th>Name</th>
              <th>State</th>
              <th>Speed</th>
              <th>Alias</th>
              <th>Type</th>
              <th>Connected to</th>
            </tr>
          </thead>
          <tbody>
            {interfaces.map((iface) => (
              <tr key={iface.id}>
                <td>{iface.displayName ?? iface.name}</td>
                <td><UpBadge up={iface.up} /></td>
                <td>{iface.speed !== null ? `${iface.speed} Mb/s` : '—'}</td>
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
                  <th>Interface</th>
                  <th>Description</th>
                </tr>
              </thead>
              <tbody>
                {sensors.map((sensor) => (
                  <tr key={`${sensor.sensorId}-${sensor.name}-${sensor.valueType}`}>
                    <td>{sensor.name || `sensor ${sensor.sensorId}`}</td>
                    <td>{formatSensorValue(sensor.value, sensor.valueType)}</td>
                    <td>{sensor.interfaceName ?? '—'}</td>
                    <td className="muted">{sensor.description || '—'}</td>
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
                  <th>Enabled</th>
                  <th>Path cost</th>
                  <th>Designated cost</th>
                  <th>Priority</th>
                  <th>Fwd transitions</th>
                </tr>
              </thead>
              <tbody>
                {stp.map((port) => (
                  <tr key={`${port.vlan}-${port.stpPortId}`}>
                    <td>{port.vlan}</td>
                    <td>{port.interfaceName ?? `port ${port.stpPortId}`}</td>
                    <td>{port.role}</td>
                    <td><StpStateBadge state={port.state} /></td>
                    <td>{port.enabled === null ? '—' : port.enabled ? 'yes' : <span className="badge badge-warn">no</span>}</td>
                    <td>{port.pathCost}</td>
                    <td>{port.designatedCost}</td>
                    <td>{port.priority}</td>
                    <td>{port.forwardTransitions}</td>
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
