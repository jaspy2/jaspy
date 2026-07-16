export function UpBadge({ up }: { up: boolean | null }) {
  if (up === true) return <span className="badge badge-ok">up</span>;
  if (up === false) return <span className="badge badge-bad">down</span>;
  return <span className="badge badge-muted">unknown</span>;
}

export function PollingBadge({ enabled }: { enabled: boolean | null }) {
  if (enabled === false) return <span className="badge badge-warn">polling off</span>;
  return <span className="badge badge-muted">{enabled === true ? 'polling on' : 'polling default'}</span>;
}

// STP port state (BRIDGE-MIB dot1dStpPortState, decoded server-side).
// "blocking" is muted, not bad: it's the normal state of redundant links.
export function StpStateBadge({ state }: { state: string }) {
  const cls =
    state === 'forwarding' ? 'badge-ok'
    : state === 'learning' || state === 'listening' ? 'badge-warn'
    : state === 'broken' ? 'badge-bad'
    : 'badge-muted';
  return <span className={`badge ${cls}`}>{state}</span>;
}

// Interface health severity ("warn"/"bad"); label is optional so it can be a
// short chip in a row or a longer word in a device list.
export function HealthBadge({ severity, label }: { severity: 'warn' | 'bad'; label?: string }) {
  const cls = severity === 'bad' ? 'badge-bad' : 'badge-warn';
  return <span className={`badge ${cls}`}>{label ?? (severity === 'bad' ? '⚠ issues' : '⚠')}</span>;
}
