import { useEffect } from 'react';
import { useQuery } from '@tanstack/react-query';
import { Link, useSearchParams } from 'react-router-dom';
import { api } from '../api/client';
import type { StpNode, StpTree } from '../api/types';
import { StpStateBadge } from '../components/StatusBadge';

// Recent topology change threshold: STP churn within this window gets a badge.
const CHURN_SECS = 15 * 60;

function formatAgo(secs: number): string {
  if (secs < 90) return `${secs}s ago`;
  if (secs < 5400) return `${Math.round(secs / 60)}m ago`;
  if (secs < 129600) return `${Math.round(secs / 3600)}h ago`;
  return `${Math.round(secs / 86400)}d ago`;
}

// One traversed link on the A->B route. The link is always some node's
// root-port link; `state` is that port's STP state.
export interface StpPathHop {
  from: string;
  fromInterface: string | null;
  to: string;
  toInterface: string | null;
  state: string | null;
}

// The STP route between two switches: A's root-ward walk and B's root-ward
// walk spliced at their lowest common ancestor. Pure so it can be unit-tested.
export function computeStpPath(tree: StpTree, from: string, to: string): { hops: StpPathHop[] } | { error: string } {
  const byFqdn = new Map(tree.nodes.map((n) => [n.fqdn, n]));
  if (!byFqdn.has(from) || !byFqdn.has(to)) return { error: 'Both switches must be part of this VLAN’s tree.' };

  const ancestors = (start: string): StpNode[] => {
    const chain: StpNode[] = [];
    const seen = new Set<string>();
    let current = byFqdn.get(start);
    while (current && !seen.has(current.fqdn)) {
      seen.add(current.fqdn);
      chain.push(current);
      current = current.parent ? byFqdn.get(current.parent) : undefined;
    }
    return chain;
  };

  const upFrom = ancestors(from);
  const upTo = ancestors(to);
  const toIndex = new Map(upTo.map((n, i) => [n.fqdn, i]));
  const lcaInFrom = upFrom.findIndex((n) => toIndex.has(n.fqdn));
  if (lcaInFrom === -1) {
    return { error: 'No common tree — the two switches are not connected on this VLAN (different roots or orphaned).' };
  }
  const lcaInTo = toIndex.get(upFrom[lcaInFrom].fqdn)!;

  const hops: StpPathHop[] = [];
  // Root-ward from A to the LCA: each hop is the child's root-port link.
  for (let i = 0; i < lcaInFrom; i++) {
    const child = upFrom[i];
    hops.push({
      from: child.fqdn,
      fromInterface: child.rootPortInterfaceName,
      to: child.parent!,
      toInterface: child.parentInterface,
      state: child.rootPortState,
    });
  }
  // Leaf-ward from the LCA to B: the same links from B's chain, reversed.
  for (let i = lcaInTo - 1; i >= 0; i--) {
    const child = upTo[i];
    hops.push({
      from: child.parent!,
      fromInterface: child.parentInterface,
      to: child.fqdn,
      toInterface: child.rootPortInterfaceName,
      state: child.rootPortState,
    });
  }
  return { hops };
}

function DeviceLink({ fqdn }: { fqdn: string }) {
  return <Link to={`/devices/${encodeURIComponent(fqdn)}`}>{fqdn}</Link>;
}

export default function STP() {
  const [params, setParams] = useSearchParams();
  const vlanParam = params.get('vlan');
  const vlan = vlanParam !== null ? Number(vlanParam) : null;
  const from = params.get('from') ?? '';
  const to = params.get('to') ?? '';

  const setParam = (key: string, value: string) => {
    setParams((prev) => {
      const next = new URLSearchParams(prev);
      if (value) next.set(key, value);
      else next.delete(key);
      return next;
    });
  };

  const summary = useQuery({ queryKey: ['stp'], queryFn: api.stp, refetchInterval: 30000 });
  const tree = useQuery({
    queryKey: ['stp', vlan],
    queryFn: () => api.stpTree(vlan!),
    enabled: vlan !== null,
    refetchInterval: 30000,
  });
  const system = useQuery({ queryKey: ['system'], queryFn: api.system, staleTime: 60000 });

  // Default to the first VLAN with data once the summary arrives.
  const vlans = summary.data ?? [];
  const firstVlan = vlans.length > 0 ? vlans[0].vlan : null;
  useEffect(() => {
    if (vlan === null && firstVlan !== null) {
      setParams((prev) => {
        const next = new URLSearchParams(prev);
        next.set('vlan', String(firstVlan));
        return next;
      }, { replace: true });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [vlan, firstVlan]);

  const nodes = tree.data?.nodes ?? [];
  const blocked = tree.data?.blockedLinks ?? [];
  const flags = tree.data?.flags ?? [];
  const path = tree.data && from && to && from !== to ? computeStpPath(tree.data, from, to) : null;

  const stpEnabled = system.data?.entitypollerEnabled !== false && system.data?.entitypollerStpEnabled !== false;

  return (
    <>
      <h1>STP</h1>
      <p className="muted">
        The active spanning tree per VLAN, computed from each switch&rsquo;s root ports and the
        discovered link topology.
      </p>
      <div className="toolbar">
        <label>
          VLAN{' '}
          <select value={vlan ?? ''} onChange={(e) => setParam('vlan', e.target.value)}>
            {vlan === null && <option value="">—</option>}
            {vlans.map((s) => (
              <option key={s.vlan} value={s.vlan}>
                {s.vlan} ({s.nodeCount} switches)
              </option>
            ))}
          </select>
        </label>
        {flags.map((flag) => (
          <span key={flag} className="badge badge-warn" title="Structural anomaly in the computed tree">
            {flag}
          </span>
        ))}
      </div>
      {summary.isError && <p className="error">Failed to load STP data: {String(summary.error)}</p>}
      {vlans.length === 0 && !summary.isLoading && (
        <p className="muted">
          {!stpEnabled
            ? 'STP polling is disabled (entitypoller / JASPY_ENTITYPOLLER_DISABLE_STP).'
            : 'No STP data (yet) — the entitypoller fills this after its first cycle.'}
        </p>
      )}

      {vlan !== null && nodes.length > 0 && (
        <>
          <h2>Tree — VLAN {vlan}</h2>
          <div className="table-wrap">
            <table>
              <thead>
                <tr>
                  <th>Switch</th>
                  <th>Uplink</th>
                  <th>State</th>
                  <th className="hide-mobile">Path cost</th>
                  <th className="hide-mobile">Root cost</th>
                  <th>Topology change</th>
                </tr>
              </thead>
              <tbody>
                {nodes.map((node) => (
                  <tr key={node.fqdn}>
                    <td className="wrap-mobile" style={{ paddingLeft: `${0.75 + node.depth * 1.5}em` }}>
                      {node.depth > 0 && <span className="muted">└ </span>}
                      <DeviceLink fqdn={node.fqdn} />
                      {node.depth === 0 && !node.orphan && <span className="badge badge-ok" style={{ marginLeft: 8 }}>root</span>}
                      {node.orphan && <span className="badge badge-warn" style={{ marginLeft: 8 }} title="Upstream could not be resolved from the link topology">orphan</span>}
                      {node.rootMismatch && (() => {
                        // A superior, off-fleet reported root is the benign
                        // "root not monitored" case, not a genuine mismatch.
                        const d = node.rootMismatchDetail;
                        const unmonitored = d?.reportedRootSuperior === true && !d.reportedRootMonitored;
                        return unmonitored ? (
                          <span className="badge badge-muted" style={{ marginLeft: 8 }} title={`The real root ${node.reported?.rootMac ?? '?'} (lower bridge ID) is not monitored by jaspy`}>root not monitored</span>
                        ) : (
                          <span className="badge badge-warn" style={{ marginLeft: 8 }} title={`This switch reports root ${node.reported?.rootMac ?? '?'}, which is not the computed root`}>root mismatch</span>
                        );
                      })()}
                    </td>
                    <td>
                      {node.rootPortInterfaceName ?? '—'}
                      {node.parentInterface && <span className="muted"> → {node.parentInterface}</span>}
                    </td>
                    <td>{node.rootPortState ? <StpStateBadge state={node.rootPortState} /> : '—'}</td>
                    <td className="hide-mobile">{node.pathCost ?? '—'}</td>
                    <td className="hide-mobile">{node.reported?.rootCost ?? '—'}</td>
                    <td>
                      {node.reported?.timeSinceTopologyChangeSecs != null ? (
                        <span className={node.reported.timeSinceTopologyChangeSecs < CHURN_SECS ? 'badge badge-warn' : 'muted'}>
                          {formatAgo(node.reported.timeSinceTopologyChangeSecs)}
                          {node.reported.topologyChanges != null && ` (${node.reported.topologyChanges}×)`}
                        </span>
                      ) : (
                        '—'
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>

          {blocked.length > 0 && (
            <>
              <h2>Blocked links ({blocked.length})</h2>
              <p className="muted">Redundant links pruned by STP; traffic fails over to these if the tree changes.</p>
              <div className="table-wrap">
                <table>
                  <thead>
                    <tr>
                      <th>Switch</th>
                      <th>Interface</th>
                      <th>Role</th>
                      <th>State</th>
                      <th className="hide-mobile">Connected to</th>
                    </tr>
                  </thead>
                  <tbody>
                    {blocked.map((link) => (
                      <tr key={`${link.fqdn}-${link.stpPortId}`}>
                        <td className="wrap-mobile"><DeviceLink fqdn={link.fqdn} /></td>
                        <td>{link.interfaceName ?? '—'}</td>
                        <td>{link.role}</td>
                        <td><StpStateBadge state={link.state} /></td>
                        <td className="hide-mobile">
                          {link.connectedTo ? (
                            <>
                              <DeviceLink fqdn={link.connectedTo.fqdn} />:{link.connectedTo.interface}
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
          )}

          <h2>Path</h2>
          <p className="muted">The route frames take between two switches on VLAN {vlan}.</p>
          <div className="toolbar">
            <label>
              From{' '}
              <select value={from} onChange={(e) => setParam('from', e.target.value)}>
                <option value="">—</option>
                {nodes.map((n) => (
                  <option key={n.fqdn} value={n.fqdn}>{n.fqdn}</option>
                ))}
              </select>
            </label>
            <label>
              To{' '}
              <select value={to} onChange={(e) => setParam('to', e.target.value)}>
                <option value="">—</option>
                {nodes.map((n) => (
                  <option key={n.fqdn} value={n.fqdn}>{n.fqdn}</option>
                ))}
              </select>
            </label>
          </div>
          {from && to && from === to && <p className="muted">Same switch — no hops.</p>}
          {path && 'error' in path && <p className="error">{path.error}</p>}
          {path && 'hops' in path && (
            <div className="table-wrap">
              <table>
                <thead>
                  <tr>
                    <th>#</th>
                    <th>From</th>
                    <th className="hide-mobile">Egress</th>
                    <th>To</th>
                    <th className="hide-mobile">Ingress</th>
                    <th>State</th>
                  </tr>
                </thead>
                <tbody>
                  {path.hops.map((hop, i) => (
                    <tr key={`${hop.from}-${hop.to}`}>
                      <td className="muted">{i + 1}</td>
                      <td className="wrap-mobile"><DeviceLink fqdn={hop.from} /></td>
                      <td className="hide-mobile">{hop.fromInterface ?? '—'}</td>
                      <td className="wrap-mobile"><DeviceLink fqdn={hop.to} /></td>
                      <td className="hide-mobile">{hop.toInterface ?? '—'}</td>
                      <td>{hop.state ? <StpStateBadge state={hop.state} /> : '—'}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </>
      )}
      {vlan !== null && nodes.length === 0 && !tree.isLoading && vlans.length > 0 && (
        <p className="muted">No STP data for VLAN {vlan}.</p>
      )}
    </>
  );
}
