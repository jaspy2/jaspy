import { useMemo, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { useQuery } from '@tanstack/react-query';
import { api } from '../api/client';
import type { Device } from '../api/types';
import { HealthBadge, PollingBadge, UpBadge } from '../components/StatusBadge';

type SortKey = 'fqdn' | 'deviceType' | 'up' | 'interfaceCount' | 'lastPoll';

function lastPollText(secs: number | null): string {
  if (secs === null) return '—';
  return `${secs}s ago`;
}

export default function Devices() {
  const navigate = useNavigate();
  const devices = useQuery({ queryKey: ['devices'], queryFn: api.devices, refetchInterval: 10000 });
  const [filter, setFilter] = useState('');
  const [sortKey, setSortKey] = useState<SortKey>('fqdn');
  const [sortAsc, setSortAsc] = useState(true);

  const rows = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    const filtered = (devices.data ?? []).filter(
      (d) =>
        !needle ||
        d.fqdn.toLowerCase().includes(needle) ||
        (d.deviceType ?? '').toLowerCase().includes(needle) ||
        (d.softwareVersion ?? '').toLowerCase().includes(needle),
    );
    const cmp = (a: Device, b: Device): number => {
      switch (sortKey) {
        case 'fqdn':
          return a.fqdn.localeCompare(b.fqdn);
        case 'deviceType':
          return (a.deviceType ?? '').localeCompare(b.deviceType ?? '');
        case 'interfaceCount':
          return a.interfaceCount - b.interfaceCount;
        case 'up': {
          const rank = (u: boolean | null) => (u === false ? 0 : u === null ? 1 : 2);
          return rank(a.up) - rank(b.up);
        }
        case 'lastPoll': {
          const val = (s: number | null) => (s === null ? Number.MAX_SAFE_INTEGER : s);
          return val(a.secondsSinceLastPoll) - val(b.secondsSinceLastPoll);
        }
      }
    };
    filtered.sort((a, b) => (sortAsc ? cmp(a, b) : -cmp(a, b)));
    return filtered;
  }, [devices.data, filter, sortKey, sortAsc]);

  const onSort = (key: SortKey) => {
    if (key === sortKey) setSortAsc(!sortAsc);
    else {
      setSortKey(key);
      setSortAsc(true);
    }
  };
  const arrow = (key: SortKey) => (sortKey === key ? (sortAsc ? ' ▲' : ' ▼') : '');

  return (
    <>
      <h1>Devices</h1>
      <div className="toolbar">
        <input
          type="search"
          placeholder="Filter by fqdn, type, software…"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        {/* On mobile the sortable table headers are hidden; offer sort here. */}
        <select
          className="mobile-only"
          aria-label="Sort devices"
          value={`${sortKey}:${sortAsc ? 'asc' : 'desc'}`}
          onChange={(e) => {
            const [key, dir] = e.target.value.split(':');
            setSortKey(key as SortKey);
            setSortAsc(dir === 'asc');
          }}
        >
          <option value="fqdn:asc">Name A–Z</option>
          <option value="fqdn:desc">Name Z–A</option>
          <option value="up:asc">Down first</option>
          <option value="lastPoll:desc">Stalest poll first</option>
          <option value="interfaceCount:desc">Most interfaces</option>
        </select>
        <span className="muted">{rows.length} devices</span>
      </div>
      {devices.isError && <p className="error">Failed to load devices: {String(devices.error)}</p>}

      <div className="item-list mobile-only">
        {rows.map((d) => (
          <div
            key={d.id}
            className="item-card clickable"
            onClick={() => navigate(`/devices/${encodeURIComponent(d.fqdn)}`)}
          >
            <span className="item-title">
              <span>{d.fqdn}</span>
              <UpBadge up={d.up} />
              {d.interfaceHealth && <HealthBadge severity={d.interfaceHealth} label="⚠ interfaces" />}
            </span>
            <span className="item-sub">
              <span>{d.deviceType ?? 'unknown type'}</span>
              {d.softwareVersion && <span>{d.softwareVersion}</span>}
            </span>
            <span className="item-sub">
              <span>{d.interfaceCount} interfaces</span>
              <span>polled {lastPollText(d.secondsSinceLastPoll)}</span>
              {d.pollingEnabled === false && <span className="badge badge-warn">polling off</span>}
            </span>
          </div>
        ))}
        {rows.length === 0 && !devices.isLoading && (
          <p className="muted">No devices. Run discovery to populate the inventory.</p>
        )}
      </div>

      <div className="table-wrap desktop-only">
        <table>
          <thead>
            <tr>
              <th className="sortable" onClick={() => onSort('fqdn')}>Device{arrow('fqdn')}</th>
              <th className="sortable" onClick={() => onSort('up')}>State{arrow('up')}</th>
              <th className="sortable" onClick={() => onSort('deviceType')}>Type{arrow('deviceType')}</th>
              <th>Software</th>
              <th>Polling</th>
              <th className="sortable" onClick={() => onSort('lastPoll')}>Last poll{arrow('lastPoll')}</th>
              <th className="sortable" onClick={() => onSort('interfaceCount')}>Interfaces{arrow('interfaceCount')}</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((d) => (
              <tr key={d.id} className="clickable" onClick={() => navigate(`/devices/${encodeURIComponent(d.fqdn)}`)}>
                <td>{d.fqdn}</td>
                <td>
                  <UpBadge up={d.up} />
                  {d.interfaceHealth && <> <HealthBadge severity={d.interfaceHealth} label="⚠ interfaces" /></>}
                </td>
                <td>{d.deviceType ?? '—'}</td>
                <td>{d.softwareVersion ?? '—'}</td>
                <td><PollingBadge enabled={d.pollingEnabled} /></td>
                <td>{lastPollText(d.secondsSinceLastPoll)}</td>
                <td>{d.interfaceCount}</td>
              </tr>
            ))}
            {rows.length === 0 && !devices.isLoading && (
              <tr>
                <td colSpan={7} className="muted">
                  No devices. Run discovery to populate the inventory.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </>
  );
}
