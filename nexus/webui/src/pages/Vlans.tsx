import { Fragment, useMemo, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { Link } from 'react-router-dom';
import { api } from '../api/client';
import type { VlanSummary } from '../api/types';

// A VLAN's display name: the single agreed name, a dash when nothing on the
// network names it, or every conflicting variant when the switches disagree.
function vlanNameCell(vlan: VlanSummary) {
  if (vlan.names.length === 0) return <span className="muted">—</span>;
  if (vlan.names.length === 1) return <span>{vlan.names[0]}</span>;
  return (
    <span>
      {vlan.names.join(' / ')}{' '}
      <span className="badge badge-warn" title="This VLAN is named differently on different switches">
        name mismatch
      </span>
    </span>
  );
}

export default function Vlans() {
  const vlans = useQuery({ queryKey: ['vlans'], queryFn: api.vlans, refetchInterval: 30000 });
  const system = useQuery({ queryKey: ['system'], queryFn: api.system, staleTime: 60000 });
  const [filter, setFilter] = useState('');
  const [expanded, setExpanded] = useState<Set<number>>(new Set());
  const toggle = (id: number) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const rows = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    return (vlans.data ?? []).filter((v) => {
      if (!needle) return true;
      return (
        String(v.id).includes(needle) ||
        v.names.some((n) => n.toLowerCase().includes(needle)) ||
        v.devices.some((d) => d.fqdn.toLowerCase().includes(needle))
      );
    });
  }, [vlans.data, filter]);

  const conflicts = (vlans.data ?? []).filter((v) => v.names.length > 1).length;

  return (
    <>
      <h1>VLANs</h1>
      <p className="muted">
        Every VLAN seen on the network by the VLAN poller, with the switches it exists on.
      </p>
      <div className="toolbar">
        <input
          type="search"
          placeholder="Filter by id, name, switch…"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <span className="muted">
          {rows.length} VLANs
          {conflicts > 0 && (
            <>
              {' · '}
              <span className="badge badge-warn">{conflicts} name mismatch{conflicts > 1 ? 'es' : ''}</span>
            </>
          )}
        </span>
      </div>
      {vlans.isError && <p className="error">Failed to load VLANs: {String(vlans.error)}</p>}
      <div className="table-wrap">
        <table>
          <thead>
            <tr>
              <th>VLAN</th>
              <th>Name</th>
              <th>Switches</th>
              <th className="hide-mobile">Native ports</th>
              <th className="hide-mobile">Tagged ports</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((vlan) => (
              <Fragment key={vlan.id}>
                <tr>
                  <td>
                    <button className="vlan-toggle" onClick={() => toggle(vlan.id)} aria-expanded={expanded.has(vlan.id)}>
                      {vlan.id} {expanded.has(vlan.id) ? '▾' : '▸'}
                    </button>
                  </td>
                  <td className="wrap-mobile">{vlanNameCell(vlan)}</td>
                  <td>{vlan.devices.length}</td>
                  <td className="hide-mobile">{vlan.devices.reduce((sum, d) => sum + d.nativePorts, 0)}</td>
                  <td className="hide-mobile">{vlan.devices.reduce((sum, d) => sum + d.taggedPorts, 0)}</td>
                </tr>
                {expanded.has(vlan.id) && (
                  <tr className="vlan-detail-row">
                    <td colSpan={5}>
                      <p style={{ margin: '4px 0 8px' }}>
                        <Link to={`/stp?vlan=${vlan.id}`}>STP tree for VLAN {vlan.id} →</Link>
                      </p>
                      <div className="vlan-list">
                        {vlan.devices.map((device) => (
                          <Fragment key={device.fqdn}>
                            <Link to={`/devices/${encodeURIComponent(device.fqdn)}`}>{device.fqdn}</Link>
                            <span className={vlan.names.length > 1 ? 'warn-text' : 'muted'}>{device.name ?? '—'}</span>
                            <span className="muted">
                              {device.nativePorts} native, {device.taggedPorts} tagged
                            </span>
                          </Fragment>
                        ))}
                      </div>
                    </td>
                  </tr>
                )}
              </Fragment>
            ))}
            {rows.length === 0 && !vlans.isLoading && (
              <tr>
                <td colSpan={5} className="muted">
                  {system.data?.vlanpollerEnabled === false
                    ? 'The VLAN poller is disabled (JASPY_ENABLE_VLANPOLLER).'
                    : 'No VLAN data (yet) — the VLAN poller fills this after its first cycle.'}
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </>
  );
}
