import { useMemo, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { Link } from 'react-router-dom';
import { api } from '../api/client';

export default function ClientLocations() {
  const locations = useQuery({ queryKey: ['clientlocations'], queryFn: api.clientLocations, refetchInterval: 30000 });
  const devices = useQuery({ queryKey: ['devices'], queryFn: api.devices, refetchInterval: 30000 });
  const [filter, setFilter] = useState('');

  const deviceById = useMemo(() => {
    const map = new Map<number, string>();
    for (const d of devices.data ?? []) map.set(d.id, d.fqdn);
    return map;
  }, [devices.data]);

  const rows = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    return (locations.data ?? [])
      .filter((l) => {
        if (!needle) return true;
        const fqdn = deviceById.get(l.deviceId) ?? '';
        return (
          l.ipAddress.toLowerCase().includes(needle) ||
          l.hwAddress.toLowerCase().includes(needle) ||
          l.portInfo.toLowerCase().includes(needle) ||
          fqdn.toLowerCase().includes(needle)
        );
      })
      .sort((a, b) => a.ipAddress.localeCompare(b.ipAddress, undefined, { numeric: true }));
  }, [locations.data, deviceById, filter]);

  return (
    <>
      <h1>Client locations</h1>
      <p className="muted">
        Client attachment points learned from DHCP option-82 (via snmptrapd/relay integrations).
      </p>
      <div className="toolbar">
        <input
          type="search"
          placeholder="Filter by IP, MAC, switch, port…"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <span className="muted">{rows.length} clients</span>
      </div>
      {locations.isError && <p className="error">Failed to load client locations: {String(locations.error)}</p>}
      <div className="table-wrap">
        <table>
          <thead>
            <tr>
              <th>Client IP</th>
              <th>Client MAC</th>
              <th>Switch</th>
              <th>Port</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((l) => {
              const fqdn = deviceById.get(l.deviceId);
              return (
                <tr key={l.id}>
                  <td>{l.ipAddress}</td>
                  <td>{l.hwAddress}</td>
                  <td>{fqdn ? <Link to={`/devices/${encodeURIComponent(fqdn)}`}>{fqdn}</Link> : `device #${l.deviceId}`}</td>
                  <td>{l.portInfo}</td>
                </tr>
              );
            })}
            {rows.length === 0 && !locations.isLoading && (
              <tr>
                <td colSpan={4} className="muted">No client locations recorded.</td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </>
  );
}
