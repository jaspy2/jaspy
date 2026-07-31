import type { InterfaceHealth, SnmpHealth } from '../api/types';

// Adaptive SNMP-polling health: "slow" (badge-warn) when the learned socket
// timeout is elevated, "not responding" (badge-bad) when the device collapsed to
// fast-fail. The effective timeout / smoothed RTT are surfaced on hover. Renders
// nothing when the device polls normally.
export function SnmpHealthBadge({ health }: { health: SnmpHealth | null | undefined }) {
  if (!health) return null;
  const secs = (health.effectiveTimeoutMs / 1000).toFixed(1);
  if (health.status === 'dead') {
    return <span className="badge badge-bad" title={`SNMP not responding (timeout ${secs}s)`}>SNMP down</span>;
  }
  const rtt = health.ewmaLatencyMs != null ? `, avg ${Math.round(health.ewmaLatencyMs)}ms` : '';
  return <span className="badge badge-warn" title={`SNMP slow: timeout raised to ${secs}s${rtt}`}>SNMP slow</span>;
}

export function UpBadge({ up }: { up: boolean | null }) {
  if (up === true) return <span className="badge badge-ok">up</span>;
  if (up === false) return <span className="badge badge-bad">down</span>;
  return <span className="badge badge-muted">unknown</span>;
}

// A port the switch has error-disabled (CISCO-ERR-DISABLE-MIB). Always a fault
// (badge-bad); the cause is surfaced in the title for a quick hover.
export function ErrDisabledBadge({ cause }: { cause: string | null }) {
  return (
    <span className="badge badge-bad" title={cause ? `err-disabled: ${cause}` : 'err-disabled'}>
      err-disabled{cause ? ` · ${cause}` : ''}
    </span>
  );
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

// High output discards get their own icon so a discard-heavy port is spottable
// at a glance, distinct from the generic ⚠ (which covers every other signal).
export function DiscardBadge() {
  return <span className="badge badge-warn" title="high outgoing discards">🗑</span>;
}

// The health badges shown for an interface in the list: a dedicated 🗑 when
// discards crossed the threshold, plus the generic ⚠/⚠ issues badge when any
// *other* signal is present. Discards-only ⇒ 🗑 alone; discards + something
// else ⇒ both; anything else ⇒ the unchanged ⚠.
export function InterfaceHealthBadges({ health }: { health: InterfaceHealth }) {
  const otherSignal =
    health.flapping ||
    health.stale ||
    health.highUtilization ||
    health.speedChangeCount > 0 ||
    health.inErrors + health.outErrors > 0;
  // A leading space before each badge spaces it inline (desktop table cell);
  // in the mobile flex row the whitespace nodes are ignored and `gap` spaces
  // them instead.
  return (
    <>
      {health.discardsHigh && <> <DiscardBadge /></>}
      {health.severity && otherSignal && <> <HealthBadge severity={health.severity} /></>}
    </>
  );
}
