import { Fragment, useState, type ReactNode } from 'react';
import { Link, useNavigate, useParams } from 'react-router-dom';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api } from '../api/client';
import type { CdpNeighbor, Device, DeviceUpdate, Interface, InterfaceHealth, InterfacePoe, LiveEvent, PoeBudget, PortChannelMember, StpPort } from '../api/types';
import { HealthBadge, PollingBadge, StpStateBadge, UpBadge } from '../components/StatusBadge';
import ActionMenu from '../components/ActionMenu';
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

// "deviceId:port" (or just deviceId) for a CDP neighbor rendered as text.
function cdpNeighborText(n: CdpNeighbor): string {
  return n.devicePort ? `${n.deviceId}:${n.devicePort}` : n.deviceId;
}

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

// Cap the tagged-VLAN column: show the collapsed range list when it's a
// handful of segments, otherwise a count (the full per-VLAN breakdown is in the
// expandable detail row). Deciding on segment count rather than raw VLAN count
// keeps a contiguous trunk compact ("1-4094" is one segment, always shown)
// while collapsing a scattered list into "N tagged VLANs".
const MAX_VLAN_SEGMENTS = 6;

export function summarizeTaggedVlans(vlans: number[]): string {
  const ranges = formatVlanRanges(vlans);
  if (ranges.split(', ').length <= MAX_VLAN_SEGMENTS) return ranges;
  return `${vlans.length} tagged VLANs`;
}

// Nominal port speed label, capability-first: read the Cisco-style name prefix
// (stable regardless of link state), falling back to the negotiated speed.
function portSpeedLabel(iface: Interface): string {
  const n = iface.displayName ?? iface.name ?? '';
  if (/^(Hu|HundredGig)/i.test(n)) return '100G';
  if (/^(Fo|FortyGig)/i.test(n)) return '40G';
  if (/^(Twe|TwentyFiveGig)/i.test(n)) return '25G';
  if (/^(Te|TenGig)/i.test(n)) return '10G';
  if (/^(Gi|GigabitEthernet)/i.test(n)) return '1G';
  if (/^(Fa|FastEthernet)/i.test(n)) return '100M';
  const s = iface.speed;
  if (s === null) return '?';
  return s >= 1000 ? `${s / 1000}G` : `${s}M`;
}

function speedRank(label: string): number {
  const m = /^(\d+(?:\.\d+)?)([MG])$/.exec(label);
  if (!m) return Number.MAX_SAFE_INTEGER; // '?' sorts last
  return parseFloat(m[1]) * (m[2] === 'G' ? 1000 : 1);
}

// --- PoE rendering --------------------------------------------------------

// Milliwatts -> "4.6 W"; null (standards-only device) -> null.
function formatWatts(mw: number | null): string | null {
  if (mw === null) return null;
  return `${Math.round(mw / 100) / 10} W`;
}

// Short word for a non-delivering PoE status.
const POE_STATUS_LABEL: Record<string, string> = {
  searching: 'searching',
  disabled: 'disabled',
  fault: 'fault',
  test: 'test',
  otherFault: 'fault',
  other: '—',
};

// PoE column cell: live draw + class when delivering, otherwise a muted status.
function PoeCell({ poe }: { poe: InterfacePoe | null }) {
  if (!poe) return <>—</>;
  if (poe.status === 'deliveringPower') {
    const watts = formatWatts(poe.powerMw);
    const cls = poe.class !== null ? ` · class ${poe.class}` : '';
    return <span title={poe.priority ? `priority ${poe.priority}` : undefined}>⚡ {watts ?? 'on'}{cls}</span>;
  }
  return <span className="muted">{POE_STATUS_LABEL[poe.status] ?? poe.status}</span>;
}

// Utilization bar colour: green under 85%, amber to 95%, red above.
function poeMeterColor(pct: number): string {
  if (pct >= 95) return '#e5534b';
  if (pct >= 85) return '#d9a406';
  return '#3fb950';
}

// Device-wide PoE budget summary (one card per PSE group), with a utilization
// meter. Shown only for PoE-capable devices.
// A titled section whose body collapses when its heading is clicked. Starts
// expanded; each section keeps its own open/closed state so they toggle
// independently (desktop and mobile alike).
function Section({ title, suffix, children }: { title: string; suffix?: ReactNode; children: ReactNode }) {
  const [open, setOpen] = useState(true);
  return (
    <section>
      <h2 className="section-head">
        <button className="section-toggle" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
          <span className="section-caret" aria-hidden="true">{open ? '▾' : '▸'}</span>
          {title}
        </button>
        {suffix}
      </h2>
      {open && children}
    </section>
  );
}

function PoeBudgetSummary({ budgets }: { budgets: PoeBudget[] }) {
  return (
    <>
      {budgets.map((b) => (
        <div key={b.group} className="panel">
          <div className="kv">
            <span>PSE</span><span>{b.group}{b.operOn ? '' : ' — not operational'}</span>
            <span>Total budget</span><span>{b.totalW} W</span>
            <span>Consumed</span><span>{b.consumedW} W ({b.utilizationPct}%)</span>
            <span>Remaining</span><span>{b.remainingW} W</span>
          </div>
          <div style={{ background: 'rgba(128,128,128,0.25)', borderRadius: 4, height: 8, overflow: 'hidden', marginTop: 8 }}>
            <div style={{ width: `${b.utilizationPct}%`, height: '100%', background: poeMeterColor(b.utilizationPct) }} />
          </div>
        </div>
      ))}
    </>
  );
}

// One-line physical-port capability summary for the device header, e.g.
// "24 × 1G copper, 4 SFP slots (2 populated)". Only counts physical Ethernet
// ports (ifType ethernetCsmacd). Buckets: copper (media "copper") and SFP cages
// (media "sfp…") get explicit form-factor words; ports with no media data are
// counted by speed alone so the summary degrades gracefully. Returns null when
// there are no physical ports.
export function summarizePorts(interfaces: Interface[]): string | null {
  const phys = interfaces.filter((i) => i.interfaceType === 'ethernetCsmacd');
  if (phys.length === 0) return null;

  const copper = phys.filter((i) => i.media === 'copper');
  const sfp = phys.filter((i) => i.media?.startsWith('sfp'));
  const unknown = phys.filter((i) => !i.media);

  const parts: string[] = [];
  const byTier = (list: Interface[], suffix: string) => {
    const counts = new Map<string, number>();
    for (const i of list) {
      const label = portSpeedLabel(i);
      counts.set(label, (counts.get(label) ?? 0) + 1);
    }
    return [...counts.entries()]
      .sort((a, b) => speedRank(a[0]) - speedRank(b[0]))
      .map(([label, n]) => `${n} × ${label}${suffix}`);
  };

  parts.push(...byTier(copper, ' copper'));
  parts.push(...byTier(unknown, ''));

  // SFP cages grouped by family (SFP / SFP+ / SFP28 / QSFP…) with a populated
  // count, so the summary distinguishes 1G SFP from 10G SFP+ slots.
  if (sfp.length > 0) {
    const groups = new Map<string, { total: number; populated: number }>();
    for (const i of sfp) {
      const kind = sfpKind(i);
      const g = groups.get(kind) ?? { total: 0, populated: 0 };
      g.total += 1;
      if (i.media!.startsWith('sfp:')) g.populated += 1;
      groups.set(kind, g);
    }
    const order = ['SFP', 'SFP+', 'SFP28', 'QSFP+', 'QSFP28'];
    const rank = (k: string) => { const i = order.indexOf(k); return i < 0 ? order.length : i; };
    for (const kind of [...groups.keys()].sort((a, b) => rank(a) - rank(b))) {
      const g = groups.get(kind)!;
      const note = g.populated > 0 ? ` (${g.populated} populated)` : '';
      parts.push(`${g.total} ${kind} slot${g.total === 1 ? '' : 's'}${note}`);
    }
  }
  return parts.length > 0 ? parts.join(', ') : null;
}

// The transceiver family implied by an optic descr — SFP+ is the 10G form of
// SFP, SFP28 the 25G form, QSFP+/QSFP28 the 40/100G forms. This is the reliable
// SFP-vs-SFP+ signal: it comes straight from the optic (e.g. "SFP-10GBase-LR"
// → SFP+, "1000BaseLX SFP" → SFP), so a 1G optic seated in a 10G cage is still
// reported as SFP, not SFP+.
function sfpKindFromDescr(descr: string): string | null {
  if (/100G/i.test(descr)) return 'QSFP28';
  if (/40G/i.test(descr)) return 'QSFP+';
  if (/25G/i.test(descr)) return 'SFP28';
  if (/10G/i.test(descr)) return 'SFP+';
  if (/1000Base|(?:^|[^0-9])1G(?![0-9])/i.test(descr)) return 'SFP';
  return null;
}

// SFP family for a cage: from the optic descr when populated, else inferred
// from the port's nominal speed (an empty 10G cage is an SFP+ slot).
function sfpKind(iface: Interface): string {
  const m = iface.media ?? '';
  if (m.startsWith('sfp:')) {
    const kind = sfpKindFromDescr(m.slice(4));
    if (kind) return kind;
  }
  switch (portSpeedLabel(iface)) {
    case '100G': return 'QSFP28';
    case '40G': return 'QSFP+';
    case '25G': return 'SFP28';
    case '10G': return 'SFP+';
    default: return 'SFP';
  }
}

// Human-friendly rendering of the raw media string for the per-interface detail
// row: "copper (RJ45)", "SFP slot (empty)", or "SFP+ (SFP-10GBase-LR)".
export function mediaLabel(media: string): string {
  if (media === 'copper') return 'copper (RJ45)';
  if (media === 'sfp') return 'SFP slot (empty)';
  const descr = media.startsWith('sfp:') ? media.slice(4).trim() : '';
  if (!descr) return 'SFP';
  const kind = sfpKindFromDescr(descr);
  return kind ? `${kind} (${descr})` : `SFP (${descr})`;
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

// "8 minutes" / "45 seconds" — short human duration for health phrasing.
function humanDuration(secs: number): string {
  if (secs < 90) return `${secs} second${secs === 1 ? '' : 's'}`;
  const mins = Math.round(secs / 60);
  return `${mins} minute${mins === 1 ? '' : 's'}`;
}

// Turn the structured health summary into the field-debugging phrasing, e.g.
// "5,512 discards in last 5 minutes" / "3 flaps in last 10 minutes, last 8
// minutes ago". Order: most-actionable first.
// Group digits with commas regardless of browser locale (toLocaleString()
// uses a space separator in many European locales, which reads as two numbers).
function groupThousands(x: number): string {
  return Math.round(x).toString().replace(/\B(?=(\d{3})+(?!\d))/g, ',');
}

// Data volume in bytes → IEC binary units (KiB/MiB/GiB/TiB, base 1024).
function formatBytes(bytes: number): string {
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i += 1; }
  return `${i === 0 ? v : v.toFixed(2)} ${units[i]}`;
}

// Throughput in bits/sec → decimal units (kbps/Mbps/Gbps, base 1000).
function formatBitrate(bps: number): string {
  const units = ['bps', 'kbps', 'Mbps', 'Gbps', 'Tbps'];
  let v = bps;
  let i = 0;
  while (v >= 1000 && i < units.length - 1) { v /= 1000; i += 1; }
  return `${i === 0 ? Math.round(v) : v.toFixed(2)} ${units[i]}`;
}

function healthLines(h: InterfaceHealth): string[] {
  const lines: string[] = [];
  const n = groupThousands;
  if (h.stale) {
    lines.push('no recent SNMP data — the device may have stopped responding (stale)');
  }
  if (h.flapCount > 0) {
    const ago = h.lastFlapSecsAgo !== null ? `, last ${humanDuration(h.lastFlapSecsAgo)} ago` : '';
    lines.push(`${h.flapCount} link flap${h.flapCount === 1 ? '' : 's'} in last ${humanDuration(h.flapWindowSecs)}${ago}`);
  }
  if (h.discards > 0) {
    lines.push(`${n(h.discards)} discard${h.discards === 1 ? '' : 's'} in last ${humanDuration(h.counterWindowSecs)}`);
  }
  if (h.inErrors + h.outErrors > 0) {
    lines.push(`${n(h.inErrors)} input / ${n(h.outErrors)} output errors in last ${humanDuration(h.counterWindowSecs)}`);
  }
  if (h.speedChangeCount > 0 && h.lastSpeedChange) {
    const [from, to] = h.lastSpeedChange;
    lines.push(`speed renegotiated ${h.speedChangeCount}× in last ${humanDuration(h.flapWindowSecs)} (${from ?? '—'}→${to} Mb/s)`);
  }
  if (h.highUtilization && h.peakUtilizationPct !== null) {
    lines.push(`peak utilization ${Math.round(h.peakUtilizationPct)}% in last ${humanDuration(h.utilWindowSecs)}`);
  }
  return lines;
}

// Full expanded view for a flagged interface: the ⚠ summary lines on top, then
// everything we know about the interface below — every health metric (including
// the ones that are perfectly fine) plus its descriptive facts. `names` maps
// VLAN ids to names, like the VLAN list.
function InterfaceDetail({ iface, names }: { iface: Interface; names: Map<number, string | null> }) {
  const h = iface.health;
  const summary = h ? healthLines(h) : [];
  const n = groupThousands;
  const rows: { label: string; value: string; cls?: string }[] = [];

  if (h) {
    const cw = humanDuration(h.counterWindowSecs);
    const fw = humanDuration(h.flapWindowSecs);
    const uw = humanDuration(h.utilWindowSecs);
    const flapVal = h.flapCount > 0 && h.lastFlapSecsAgo !== null
      ? `${h.flapCount} (last ${humanDuration(h.lastFlapSecsAgo)} ago)`
      : String(h.flapCount);
    const speedVal = h.speedChangeCount > 0 && h.lastSpeedChange
      ? `${h.speedChangeCount} (${h.lastSpeedChange[0] ?? '—'}→${h.lastSpeedChange[1]} Mb/s)`
      : String(h.speedChangeCount);
    // "<cumulative total> (<delta in window>)" when the raw counter is known,
    // else just the windowed delta.
    const counterVal = (total: number | null, delta: number) =>
      total !== null ? `${n(total)} (${n(delta)} in last ${cw})` : `${n(delta)} in last ${cw}`;
    rows.push({ label: 'Discards', value: counterVal(iface.outDiscards, h.discards), cls: h.discards > 0 ? 'warn-text' : undefined });
    rows.push({ label: 'Input errors', value: counterVal(iface.inErrors, h.inErrors), cls: h.inErrors > 0 ? 'warn-text' : undefined });
    rows.push({ label: 'Output errors', value: counterVal(iface.outErrors, h.outErrors), cls: h.outErrors > 0 ? 'warn-text' : undefined });
    rows.push({ label: `Link flaps (last ${fw})`, value: flapVal, cls: h.flapCount > 0 ? 'bad-text' : undefined });
    rows.push({ label: `Speed changes (last ${fw})`, value: speedVal, cls: h.speedChangeCount > 0 ? 'warn-text' : undefined });
    rows.push({ label: `Peak utilization (last ${uw})`, value: h.peakUtilizationPct !== null ? `${Math.round(h.peakUtilizationPct)}%` : '—', cls: h.highUtilization ? 'warn-text' : undefined });
    const tw = humanDuration(h.throughputWindowSecs);
    rows.push({ label: `Throughput in (${tw} avg)`, value: h.rxBpsAvg !== null ? formatBitrate(h.rxBpsAvg) : '—' });
    rows.push({ label: `Throughput out (${tw} avg)`, value: h.txBpsAvg !== null ? formatBitrate(h.txBpsAvg) : '—' });
    rows.push({ label: 'SNMP data', value: h.stale ? 'stale — device not responding' : 'current', cls: h.stale ? 'bad-text' : undefined });
  }

  // Descriptive facts (everything else the API knows about the interface).
  rows.push({ label: 'ifIndex', value: String(iface.index) });
  rows.push({ label: 'Oper status', value: iface.up === true ? 'up' : iface.up === false ? 'down' : 'unknown' });
  rows.push({ label: 'Type', value: iface.interfaceType });
  if (iface.media) rows.push({ label: 'Media', value: mediaLabel(iface.media) });
  if (iface.poe) {
    const poe = iface.poe;
    const parts: string[] = [POE_STATUS_LABEL[poe.status] ?? (poe.status === 'deliveringPower' ? 'delivering power' : poe.status)];
    const watts = formatWatts(poe.powerMw);
    if (watts) parts.push(`${watts} drawn`);
    if (poe.class !== null) parts.push(`class ${poe.class}`);
    if (!poe.adminEnabled) parts.push('admin disabled');
    if (poe.priority) parts.push(`${poe.priority} priority`);
    rows.push({ label: 'PoE', value: parts.join(' · ') });
  }
  rows.push({ label: 'Speed', value: iface.speed !== null ? `${iface.speed} Mb/s` : '—' });
  if (iface.speedOverride !== null) rows.push({ label: 'Speed override', value: `${iface.speedOverride} Mb/s` });
  if (iface.inOctets !== null) rows.push({ label: 'Received (total)', value: formatBytes(iface.inOctets) });
  if (iface.outOctets !== null) rows.push({ label: 'Transmitted (total)', value: formatBytes(iface.outOctets) });
  if (iface.portChannel) rows.push({ label: 'Port-channel', value: iface.portChannel });
  if (iface.alias) rows.push({ label: 'Alias', value: iface.alias });
  if (iface.description) rows.push({ label: 'Description', value: iface.description });
  if (iface.connectedTo) rows.push({ label: 'Connected to', value: `${iface.connectedTo.fqdn}:${iface.connectedTo.interface}` });
  else if (iface.cdpNeighbor) rows.push({ label: 'CDP neighbor', value: cdpNeighborText(iface.cdpNeighbor) });
  rows.push({ label: 'Polling', value: iface.pollingEnabled === false ? 'disabled' : 'enabled' });

  return (
    <div className="iface-detail-panel">
      {summary.length > 0 && (
        <ul className="health-detail">
          {summary.map((line, i) => <li key={i} className="warn-text">⚠ {line}</li>)}
        </ul>
      )}
      <dl className="iface-detail">
        {rows.map((r, i) => (
          <Fragment key={i}>
            <dt>{r.label}</dt>
            <dd className={r.cls}>{r.value}</dd>
          </Fragment>
        ))}
      </dl>
      {(iface.nativeVlan !== null || (iface.taggedVlans?.length ?? 0) > 0) && (
        <>
          <div className="iface-detail-heading">VLAN membership</div>
          <InterfaceVlanList iface={iface} names={names} />
        </>
      )}
    </div>
  );
}

// Machine-matchable warning codes from the backend (see
// lagpoller::port_channel_warnings) rendered as readable text.
function portChannelWarningText(code: string): string {
  const [kind, detail] = code.split(':', 2) as [string, string | undefined];
  switch (kind) {
    case 'not-lacp':
      return `not running LACP (${detail})`;
    case 'single-member':
      return 'only one member port';
    case 'member-no-lacp-partner':
      return `${detail}: the far end is not running LACP on this link`;
    case 'member-not-bundled':
      return `${detail}: configured but not bundled`;
    case 'members-report-different-partners':
      return 'members are bundled to different switches (LACP partner ids differ)';
    case 'members-wired-to-different-devices':
      return 'members are cabled to different devices';
    case 'far-end-lag-not-found':
      return 'the far-end switch has no matching port-channel';
    case 'far-end-member-count-mismatch':
      return 'the far-end port-channel has a different member count';
    default:
      return code;
  }
}

function MemberStateBadge({ member }: { member: PortChannelMember }) {
  if (member.bundled) return <span className="badge badge-ok">bundled</span>;
  if (member.actorState.some((s) => s === 'defaulted' || s === 'expired')) {
    return <span className="badge badge-bad">no partner</span>;
  }
  if (member.actorState.length === 0) return <span className="badge badge-muted">no LACP</span>;
  return <span className="badge badge-warn">negotiating</span>;
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

// --- Column sorting -------------------------------------------------------
// Both detail tables sort the same way as the Devices list: click a header to
// sort, click again to flip direction. The sort key extracts a comparable
// value; a null value always sorts last regardless of direction.

type IfaceSortKey = 'name' | 'state' | 'speed' | 'vlan' | 'tagged' | 'poe' | 'alias' | 'type' | 'connected';
type StpSortKey = 'vlan' | 'interface' | 'role' | 'state' | 'enabled' | 'pathCost' | 'designatedCost' | 'priority' | 'forwardTransitions';

// Comparator that keeps nulls last, sorts numbers numerically and strings with
// natural (numeric-aware) ordering, then applies the direction.
function makeCmp<T>(val: (t: T) => string | number | null, asc: boolean): (a: T, b: T) => number {
  return (a, b) => {
    const av = val(a);
    const bv = val(b);
    if (av === null && bv === null) return 0;
    if (av === null) return 1;
    if (bv === null) return -1;
    const base =
      typeof av === 'number' && typeof bv === 'number'
        ? av - bv
        : String(av).localeCompare(String(bv), undefined, { numeric: true });
    return asc ? base : -base;
  };
}

function ifaceSortVal(i: Interface, key: IfaceSortKey): string | number | null {
  switch (key) {
    case 'name': return i.displayName ?? i.name ?? '';
    case 'state': return i.up === false ? 0 : i.up === null ? 1 : 2; // down first
    case 'speed': return i.speed;
    case 'vlan': return i.nativeVlan;
    case 'tagged': return i.taggedVlans?.length ?? 0;
    case 'poe': return i.poe && i.poe.status === 'deliveringPower' ? i.poe.powerMw ?? 0 : null;
    case 'alias': return i.alias ?? null;
    case 'type': return i.interfaceType;
    case 'connected': return i.connectedTo?.fqdn ?? (i.cdpNeighbor ? cdpNeighborText(i.cdpNeighbor) : null);
  }
}

// The filter/label key for an interface. Mostly the raw ifType, except
// propVirtual is split by name into port-channels (Po) and SVIs (VLAN) so those
// can be filtered separately from the remaining virtual interfaces.
function ifaceCategory(i: Interface): string {
  if (i.interfaceType === 'propVirtual') {
    const name = i.displayName ?? i.name ?? '';
    if (/^Po/i.test(name)) return 'Po';
    if (/^Vl/i.test(name)) return 'VLAN';
  }
  return i.interfaceType;
}

// Short chip labels; unknown categories show verbatim (so Po/VLAN pass through
// unchanged). The full category is kept as the filter key and in the tooltip.
const SHORT_IFTYPE: Record<string, string> = {
  ethernetCsmacd: 'eth',
  propVirtual: 'propV',
  l2vlan: 'l2v',
};
function shortIfType(t: string): string {
  return SHORT_IFTYPE[t] ?? t;
}

function stpSortVal(p: StpPort, key: StpSortKey): string | number | null {
  switch (key) {
    case 'vlan': return p.vlan;
    case 'interface': return p.interfaceName ?? `port ${p.stpPortId}`;
    case 'role': return p.role;
    case 'state': return p.state;
    case 'enabled': return p.enabled === null ? null : p.enabled ? 1 : 0;
    case 'pathCost': return p.pathCost;
    case 'designatedCost': return p.designatedCost;
    case 'priority': return p.priority;
    case 'forwardTransitions': return p.forwardTransitions;
  }
}

export default function DeviceDetail() {
  const { fqdn = '' } = useParams();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [confirmDelete, setConfirmDelete] = useState(false);
  // Interface ids whose VLAN list is expanded (desktop row and mobile card).
  // Every interface row expands into the full InterfaceDetail view.
  const [expandedDetail, setExpandedDetail] = useState<Set<number>>(new Set());
  const toggleDetail = (id: number) =>
    setExpandedDetail((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const [ifaceSort, setIfaceSort] = useState<IfaceSortKey>('name');
  const [ifaceAsc, setIfaceAsc] = useState(true);
  const onIfaceSort = (key: IfaceSortKey) => {
    if (key === ifaceSort) setIfaceAsc(!ifaceAsc);
    else { setIfaceSort(key); setIfaceAsc(true); }
  };
  const ifaceArrow = (key: IfaceSortKey) => (ifaceSort === key ? (ifaceAsc ? ' ▲' : ' ▼') : '');

  // Interface categories the user has toggled off. VLAN SVIs are hidden by
  // default — they're rarely what you're looking at on a device page.
  const [hiddenTypes, setHiddenTypes] = useState<Set<string>>(new Set(['VLAN']));
  const toggleType = (t: string) =>
    setHiddenTypes((prev) => {
      const next = new Set(prev);
      if (next.has(t)) next.delete(t);
      else next.add(t);
      return next;
    });

  const [stpSort, setStpSort] = useState<StpSortKey>('vlan');
  const [stpAsc, setStpAsc] = useState(true);
  const onStpSort = (key: StpSortKey) => {
    if (key === stpSort) setStpAsc(!stpAsc);
    else { setStpSort(key); setStpAsc(true); }
  };
  const stpArrow = (key: StpSortKey) => (stpSort === key ? (stpAsc ? ' ▲' : ' ▼') : '');

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

  // Single-device discovery runs on its own non-blocking lane and returns 202
  // ("queued"); it finishes in a few seconds, so nudge a refetch shortly after
  // rather than waiting on the poll. Not gated on the global discovery
  // "running" flag — that's the whole point of the separate lane.
  const runDiscovery = useMutation({
    mutationFn: () => api.runSingleDeviceDiscovery(fqdn),
    onSuccess: () => {
      setTimeout(() => queryClient.invalidateQueries({ queryKey: ['device', fqdn] }), 4000);
    },
  });

  if (detail.isLoading) return <p>Loading…</p>;
  if (detail.isError || !detail.data) return <p className="error">Failed to load {fqdn}: {String(detail.error)}</p>;

  const { device, interfaces } = detail.data;
  const portSummary = summarizePorts(interfaces);
  const poeBudget = detail.data.poeBudget ?? [];
  const vlanNames = new Map((detail.data.vlans ?? []).map((v) => [v.id, v.name]));
  const portChannels = detail.data.portChannels ?? [];
  const deviceIssues = detail.data.issues ?? [];
  const sensors = entity.data?.sensors ?? [];
  const stp = entity.data?.stp ?? [];
  const sortedInterfaces = [...interfaces].sort(makeCmp((i) => ifaceSortVal(i, ifaceSort), ifaceAsc));
  const sortedStp = [...stp].sort(makeCmp((p) => stpSortVal(p, stpSort), stpAsc));

  // Interface-type filter: one toggle per distinct ifType, alpha-sorted, with a
  // per-type count. Hidden types drop out of both the mobile and desktop views.
  const typeCounts = new Map<string, number>();
  for (const i of interfaces) {
    const cat = ifaceCategory(i);
    typeCounts.set(cat, (typeCounts.get(cat) ?? 0) + 1);
  }
  const interfaceTypes = [...typeCounts.keys()].sort();
  const visibleInterfaces = sortedInterfaces.filter((i) => !hiddenTypes.has(ifaceCategory(i)));
  const ifaceCountLabel =
    visibleInterfaces.length === interfaces.length
      ? `${interfaces.length}`
      : `${visibleInterfaces.length} of ${interfaces.length}`;
  const typeFilter =
    interfaceTypes.length > 1
      ? interfaceTypes.map((t) => {
          const on = !hiddenTypes.has(t);
          return (
            <button
              key={t}
              className={`chip ${on ? 'chip-on' : 'chip-off'}`}
              aria-pressed={on}
              onClick={() => toggleType(t)}
              title={on ? `Hide ${t} interfaces` : `Show ${t} interfaces`}
            >
              {shortIfType(t)} ({typeCounts.get(t)})
            </button>
          );
        })
      : undefined;
  const entitypollerEnabled = system.data?.entitypollerEnabled === true;
  const vlanpollerEnabled = system.data?.vlanpollerEnabled === true;
  // Megaexcel entity page keys off the device's short name (fqdn without the
  // domain suffix), e.g. "ticket-sw2.asm.fi" -> ".../entity/ticket-sw2".
  const megaexcelUrl = system.data?.megaexcelUrl ?? null;
  const megaexcelDeviceUrl = megaexcelUrl ? `${megaexcelUrl}/entity/${device.fqdn.split('.')[0]}` : null;

  return (
    <>
      <h1>
        {device.fqdn} <UpBadge up={device.up} />
      </h1>
      <div className="panel">
        <div className="kv">
          <span>Type</span><span>{device.deviceType ?? '—'}</span>
          {portSummary && (<><span>Ports</span><span className="wrap">{portSummary}</span></>)}
          <span>IP address</span><span>{(detail.data.ipAddresses ?? []).join(', ') || '—'}</span>
          <span>Software</span><span>{device.softwareVersion ?? '—'}</span>
          <span>OS info</span><span className="wrap">{device.osInfo ?? '—'}</span>
          <span>Base MAC</span><span>{device.baseMac ?? '—'}</span>
          <span>SNMP community</span><span>{device.snmpCommunity ? '••••••' : '—'}</span>
          <span>Polling</span><span><PollingBadge enabled={device.pollingEnabled} /></span>
        </div>
        <div className="actions">
          {vlanpollerEnabled && (
            <button onClick={() => pollVlans.mutate()} disabled={pollVlans.isPending}>
              Poll VLANs now
            </button>
          )}
          {megaexcelDeviceUrl && (
            <a className="button" href={megaexcelDeviceUrl} target="_blank" rel="noreferrer">
              Open in Megaexcel ↗
            </a>
          )}
          {!confirmDelete ? (
            <ActionMenu
              label="More"
              items={[
                {
                  label: device.pollingEnabled === false ? 'Enable polling' : 'Disable polling',
                  onClick: () => togglePolling.mutate(device),
                  disabled: togglePolling.isPending,
                },
                {
                  label: runDiscovery.isPending ? 'Running…' : 'Run discovery',
                  onClick: () => runDiscovery.mutate(),
                  disabled: runDiscovery.isPending,
                },
                {
                  label: 'Delete device…',
                  onClick: () => setConfirmDelete(true),
                  danger: true,
                },
              ]}
            />
          ) : (
            <>
              <button className="danger" onClick={() => deleteDevice.mutate()} disabled={deleteDevice.isPending}>
                Confirm delete {device.fqdn}
              </button>
              <button onClick={() => setConfirmDelete(false)}>Cancel</button>
            </>
          )}
          {runDiscovery.isSuccess && !runDiscovery.isPending && (
            <span className="ok">Discovery queued…</span>
          )}
          {(togglePolling.isError || deleteDevice.isError || pollVlans.isError || runDiscovery.isError) && (
            <span className="error">{String(togglePolling.error ?? deleteDevice.error ?? pollVlans.error ?? runDiscovery.error)}</span>
          )}
        </div>
      </div>

      {deviceIssues.length > 0 && (
        <Section
          title={`Issues (${deviceIssues.length})`}
          suffix={
            <span className="section-suffix">
              <Link to="/issues">open Issues →</Link>
            </span>
          }
        >
          <div className="item-list">
            {deviceIssues.map((issue) => (
              <div key={issue.issueKey} className={`panel ${issue.acknowledged ? 'issue-acked' : ''}`}>
                <span className="item-title">
                  <HealthBadge severity={issue.severity} label={issue.severity === 'bad' ? '⚠ critical' : '⚠ warning'} />
                  <span>{issue.title}</span>
                  {issue.subjectLabel && <span className="muted">{issue.subjectLabel}</span>}
                  {issue.acknowledged && <span className="badge badge-muted">acknowledged</span>}
                </span>
                <p className="issue-line">{issue.description}</p>
              </div>
            ))}
          </div>
        </Section>
      )}

      {portChannels.length > 0 && (
        <Section title={`Port-channels (${portChannels.length})`}>
          {portChannels.map((po) => (
            <div key={po.ifindex} className="panel">
              <span className="item-title">
                <span>{po.name ?? `ifIndex ${po.ifindex}`}</span>
                <UpBadge up={po.up} />
                <span className={`badge ${po.protocol === 'lacp' ? 'badge-ok' : 'badge-warn'}`}>{po.protocol}</span>
                {po.alias && <span className="muted">{po.alias}</span>}
                {po.partnerSystemId && <span className="muted">partner {po.partnerSystemId}</span>}
              </span>
              {po.warnings.length > 0 && (
                <ul className="po-warnings">
                  {po.warnings.map((code) => (
                    <li key={code} className="warn-text">
                      ⚠ {portChannelWarningText(code)}
                    </li>
                  ))}
                </ul>
              )}
              <div className="table-wrap">
                <table>
                  <thead>
                    <tr>
                      <th>Member</th>
                      <th>Status</th>
                      <th>Connected to</th>
                      <th className="hide-mobile">Partner port</th>
                      <th className="hide-mobile">LACP state</th>
                    </tr>
                  </thead>
                  <tbody>
                    {po.members.map((member) => (
                      <tr key={member.ifindex}>
                        <td className="wrap-mobile">{member.name ?? `ifIndex ${member.ifindex}`}</td>
                        <td><MemberStateBadge member={member} /></td>
                        <td className="wrap-mobile">
                          {member.connectedTo ? (
                            <>
                              <Link to={`/devices/${encodeURIComponent(member.connectedTo.fqdn)}`}>
                                {member.connectedTo.fqdn.split('.')[0]}
                              </Link>
                              :{member.connectedTo.interface}
                            </>
                          ) : (
                            '—'
                          )}
                        </td>
                        <td className="hide-mobile">{member.partnerPort ?? '—'}</td>
                        <td className="hide-mobile muted">{member.actorState.length > 0 ? member.actorState.join(', ') : '—'}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          ))}
        </Section>
      )}

      <Section title={`Interfaces (${ifaceCountLabel})`} suffix={typeFilter}>
      {/* On mobile the sortable table headers are hidden; offer sort here. */}
      <div className="toolbar mobile-only">
        <select
          aria-label="Sort interfaces"
          value={`${ifaceSort}:${ifaceAsc ? 'asc' : 'desc'}`}
          onChange={(e) => {
            const [key, dir] = e.target.value.split(':');
            setIfaceSort(key as IfaceSortKey);
            setIfaceAsc(dir === 'asc');
          }}
        >
          <option value="name:asc">Name A–Z</option>
          <option value="name:desc">Name Z–A</option>
          <option value="state:asc">Down first</option>
          <option value="speed:desc">Fastest first</option>
          <option value="vlan:asc">VLAN</option>
          <option value="poe:desc">Most PoE draw</option>
          <option value="connected:asc">Connected to</option>
        </select>
      </div>
      <div className="item-list mobile-only">
        {visibleInterfaces.map((iface) => (
          <div key={iface.id} className="item-card">
            <span className="item-title">
              <button className="detail-toggle" onClick={() => toggleDetail(iface.id)} aria-expanded={expandedDetail.has(iface.id)}>
                {iface.displayName ?? iface.name}
                {expandedDetail.has(iface.id) ? ' ▾' : ' ▸'}
              </button>
              {iface.portChannel && <span className="badge badge-muted">{iface.portChannel}</span>}
              <UpBadge up={iface.up} />
              {iface.health?.severity && <HealthBadge severity={iface.health.severity} />}
            </span>
            <span className="item-sub">
              {iface.speed !== null && <span>{iface.speed} Mb/s</span>}
              {(iface.nativeVlan !== null || (iface.taggedVlans?.length ?? 0) > 0) && (
                <span>
                  VLAN {iface.nativeVlan ?? '—'}
                  {(iface.taggedVlans?.length ?? 0) > 0 && ` (+${iface.taggedVlans!.length} tagged)`}
                </span>
              )}
              {iface.poe && iface.poe.status === 'deliveringPower' && (
                <span>PoE <PoeCell poe={iface.poe} /></span>
              )}
              {iface.alias && <span>{iface.alias}</span>}
            </span>
            {iface.connectedTo ? (
              <span className="item-sub">
                <span>
                  →{' '}
                  <Link to={`/devices/${encodeURIComponent(iface.connectedTo.fqdn)}`}>
                    {iface.connectedTo.fqdn}
                  </Link>
                  :{iface.connectedTo.interface}
                </span>
              </span>
            ) : iface.cdpNeighbor ? (
              <span className="item-sub">
                <span title="CDP neighbor (not monitored by jaspy)">
                  → {cdpNeighborText(iface.cdpNeighbor)} <span className="muted">(CDP)</span>
                </span>
              </span>
            ) : null}
            {expandedDetail.has(iface.id) && (
              <InterfaceDetail iface={iface} names={vlanNames} />
            )}
          </div>
        ))}
      </div>
      <div className="table-wrap desktop-only">
        <table>
          <thead>
            <tr>
              <th className="sortable" onClick={() => onIfaceSort('name')}>Name{ifaceArrow('name')}</th>
              <th className="sortable" onClick={() => onIfaceSort('state')}>State{ifaceArrow('state')}</th>
              <th className="sortable" onClick={() => onIfaceSort('speed')}>Speed{ifaceArrow('speed')}</th>
              <th className="sortable" onClick={() => onIfaceSort('vlan')}>VLAN{ifaceArrow('vlan')}</th>
              <th className="sortable" onClick={() => onIfaceSort('tagged')}>Tagged VLANs{ifaceArrow('tagged')}</th>
              <th className="sortable" onClick={() => onIfaceSort('poe')}>PoE{ifaceArrow('poe')}</th>
              <th className="sortable" onClick={() => onIfaceSort('alias')}>Alias{ifaceArrow('alias')}</th>
              <th className="sortable" onClick={() => onIfaceSort('type')}>Type{ifaceArrow('type')}</th>
              <th className="sortable" onClick={() => onIfaceSort('connected')}>Connected to{ifaceArrow('connected')}</th>
            </tr>
          </thead>
          <tbody>
            {visibleInterfaces.map((iface) => (
              <Fragment key={iface.id}>
                <tr>
                  <td>
                    <button className="detail-toggle" onClick={() => toggleDetail(iface.id)} aria-expanded={expandedDetail.has(iface.id)}>
                      {iface.displayName ?? iface.name}
                      {expandedDetail.has(iface.id) ? ' ▾' : ' ▸'}
                    </button>
                    {iface.portChannel && <> <span className="badge badge-muted">{iface.portChannel}</span></>}
                  </td>
                  <td>
                    <UpBadge up={iface.up} />
                    {iface.health?.severity && <> <HealthBadge severity={iface.health.severity} /></>}
                  </td>
                  <td>{iface.speed !== null ? `${iface.speed} Mb/s` : '—'}</td>
                  <td>{iface.nativeVlan ?? '—'}</td>
                  <td className="vlan-cell" title={iface.taggedVlans?.join(', ')}>
                    {(iface.taggedVlans?.length ?? 0) > 0 ? summarizeTaggedVlans(iface.taggedVlans!) : '—'}
                  </td>
                  <td><PoeCell poe={iface.poe} /></td>
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
                    ) : iface.cdpNeighbor ? (
                      <span title="CDP neighbor (not monitored by jaspy)">
                        {cdpNeighborText(iface.cdpNeighbor)} <span className="muted">(CDP)</span>
                      </span>
                    ) : (
                      '—'
                    )}
                  </td>
                </tr>
                {expandedDetail.has(iface.id) && (
                  <tr className="vlan-detail-row">
                    <td colSpan={9}>
                      <InterfaceDetail iface={iface} names={vlanNames} />
                    </td>
                  </tr>
                )}
              </Fragment>
            ))}
          </tbody>
        </table>
      </div>
      </Section>

      {poeBudget.length > 0 && (
        <Section title="PoE budget">
          <PoeBudgetSummary budgets={poeBudget} />
        </Section>
      )}

      {sensors.length > 0 && (
        <Section
          title={`Sensors (${sensors.length})`}
          suffix={<UpdatedAgo timestamps={sensors.map((s) => s.timestamp)} />}
        >
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
        </Section>
      )}

      {stp.length > 0 && (
        <Section
          title={`STP (${stp.length})`}
          suffix={<UpdatedAgo timestamps={stp.map((p) => p.timestamp)} />}
        >
          <div className="table-wrap">
            <table>
              <thead>
                <tr>
                  <th className="sortable" onClick={() => onStpSort('vlan')}>VLAN{stpArrow('vlan')}</th>
                  <th className="sortable" onClick={() => onStpSort('interface')}>Interface{stpArrow('interface')}</th>
                  <th className="sortable" onClick={() => onStpSort('role')}>Role{stpArrow('role')}</th>
                  <th className="sortable" onClick={() => onStpSort('state')}>State{stpArrow('state')}</th>
                  <th className="sortable hide-mobile" onClick={() => onStpSort('enabled')}>Enabled{stpArrow('enabled')}</th>
                  <th className="sortable hide-mobile" onClick={() => onStpSort('pathCost')}>Path cost{stpArrow('pathCost')}</th>
                  <th className="sortable hide-mobile" onClick={() => onStpSort('designatedCost')}>Designated cost{stpArrow('designatedCost')}</th>
                  <th className="sortable hide-mobile" onClick={() => onStpSort('priority')}>Priority{stpArrow('priority')}</th>
                  <th className="sortable hide-mobile" onClick={() => onStpSort('forwardTransitions')}>Fwd transitions{stpArrow('forwardTransitions')}</th>
                </tr>
              </thead>
              <tbody>
                {sortedStp.map((port) => (
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
        </Section>
      )}

      {sensors.length === 0 && stp.length === 0 && entitypollerEnabled && (
        <p className="muted">No sensor/STP data for this device (yet).</p>
      )}
    </>
  );
}
