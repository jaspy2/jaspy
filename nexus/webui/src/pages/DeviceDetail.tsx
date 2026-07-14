import { useState } from 'react';
import { Link, useNavigate, useParams } from 'react-router-dom';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api } from '../api/client';
import type { Device, DeviceUpdate, LiveEvent } from '../api/types';
import { PollingBadge, UpBadge } from '../components/StatusBadge';
import useLiveSocket from '../hooks/useLiveSocket';

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
    </>
  );
}
