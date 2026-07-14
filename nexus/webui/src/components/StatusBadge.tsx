export function UpBadge({ up }: { up: boolean | null }) {
  if (up === true) return <span className="badge badge-ok">up</span>;
  if (up === false) return <span className="badge badge-bad">down</span>;
  return <span className="badge badge-muted">unknown</span>;
}

export function PollingBadge({ enabled }: { enabled: boolean | null }) {
  if (enabled === false) return <span className="badge badge-warn">polling off</span>;
  return <span className="badge badge-muted">{enabled === true ? 'polling on' : 'polling default'}</span>;
}
