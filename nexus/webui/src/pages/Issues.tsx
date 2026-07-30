import { Fragment, useMemo, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { Link } from 'react-router-dom';
import { api } from '../api/client';
import type { Issue, IssueDetailValue, IssueType } from '../api/types';
import { HealthBadge } from '../components/StatusBadge';

// Category display order and labels for the "Issue types" management panel.
const CATEGORY_ORDER = ['device', 'poe', 'interface', 'stp', 'lag'] as const;
const CATEGORY_LABEL: Record<string, string> = {
  device: 'Device',
  poe: 'PoE',
  interface: 'Interface',
  stp: 'Spanning tree',
  lag: 'Link aggregation',
};

// Compact "5m ago" from an epoch-ms timestamp.
function ago(ms: number): string {
  const s = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ${m % 60}m ago`;
  const d = Math.floor(h / 24);
  return `${d}d ${h % 24}h ago`;
}

function absolute(ms: number): string {
  return new Date(ms).toLocaleString();
}

// The affected device, linked when it is a real device (network-level STP
// issues carry no fqdn).
function deviceCell(issue: Issue) {
  if (!issue.fqdn) return <span className="muted">network</span>;
  return <Link to={`/devices/${encodeURIComponent(issue.fqdn)}`}>{issue.hostname || issue.fqdn}</Link>;
}

// The Device cell for a row: for a combined link fault, both ends as linked
// hostnames joined by "↔"; for a single issue, the plain device cell.
function rowDeviceCell(row: Row) {
  if (row.members.length < 2) return deviceCell(row.primary);
  return (
    <span className="issue-link-ends">
      {row.members.map((m, i) => (
        <Fragment key={m.issueKey}>
          {i > 0 && <span className="muted"> ↔ </span>}
          <Link to={`/devices/${encodeURIComponent(m.fqdn)}`}>{m.hostname || m.fqdn}</Link>
        </Fragment>
      ))}
    </span>
  );
}

// A combined row acks both ends at once, so a mixed state is rare (only if one
// end's occurrence changed between acks). Surface it quietly rather than
// silently showing the row as active.
function partialAckHint(row: Row) {
  if (row.members.length < 2) return null;
  const acked = row.members.filter((m) => m.acknowledged).length;
  if (acked === 0 || acked === row.members.length) return null;
  return <span className="muted"> · {acked}/{row.members.length} acked</span>;
}

// The subject portion of an issueKey ("<fqdn>|<kind>|<subject>"); used to find
// sibling issues on the same sub-entity (e.g. the same VLAN).
function subjectOf(issueKey: string): string {
  const parts = issueKey.split('|');
  return parts.length >= 3 ? parts.slice(2).join('|') : '';
}

// Column sort for the issue table. Mirrors the makeCmp idiom used on the
// Devices / DeviceDetail pages (null-last, numeric-aware string compare).
type IssueSortKey = 'severity' | 'device' | 'affected' | 'issue' | 'age';

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

// A row in the issue list: either a single issue, or the two ends of one
// inter-switch link fault combined (issues sharing a groupKey). Grouping is a
// /issues-page presentation over the flat API list — each end keeps its own
// ack key, so acking a combined row fans out to every member.
type Row = {
  key: string; // the shared groupKey for a combined row, else the issueKey
  members: Issue[]; // one, or the ends of a link (sorted by hostname)
  primary: Issue; // representative for kind/title/explain (members share kind)
};

// Bucket a flat issue list into rows: issues with a groupKey combine (members
// sorted by hostname so the "A ↔ B" label and the per-end detail read in the
// same order); everything else is its own single-issue row. A group that ends
// up with a single member (only one end tripped) reads exactly like a normal
// single-device row.
function buildRows(list: Issue[]): Row[] {
  const groups = new Map<string, Issue[]>();
  const rows: Row[] = [];
  for (const issue of list) {
    if (issue.groupKey) {
      const arr = groups.get(issue.groupKey);
      if (arr) arr.push(issue);
      else groups.set(issue.groupKey, [issue]);
    } else {
      rows.push({ key: issue.issueKey, members: [issue], primary: issue });
    }
  }
  for (const [key, members] of groups) {
    members.sort((a, b) =>
      (a.hostname || a.fqdn).localeCompare(b.hostname || b.fqdn, undefined, { numeric: true })
    );
    rows.push({ key, members, primary: members[0] });
  }
  return rows;
}

const rowSeverity = (r: Row): 'warn' | 'bad' => (r.members.some((m) => m.severity === 'bad') ? 'bad' : 'warn');
const rowFirstSeen = (r: Row) => Math.min(...r.members.map((m) => m.firstSeen));
const rowAcked = (r: Row) => r.members.every((m) => m.acknowledged);
const rowKeys = (r: Row) => r.members.map((m) => m.issueKey);
const isCombined = (r: Row) => r.members.length > 1;

// "f04-sw1 ↔ rkh74-sw1" for a combined row; the single hostname otherwise.
function rowDeviceLabel(r: Row): string {
  if (!isCombined(r)) return r.primary.hostname || r.primary.fqdn || 'network';
  return r.members.map((m) => m.hostname || m.fqdn).join(' ↔ ');
}
// "Gi1/0/1 ↔ Gi1/0/24" for a combined row; the single affected label otherwise.
function rowAffectedLabel(r: Row): string | null {
  if (!isCombined(r)) return r.primary.subjectLabel ?? null;
  const parts = r.members.map((m) => m.subjectLabel).filter((s): s is string => !!s);
  return parts.length ? parts.join(' ↔ ') : null;
}

function rowSortVal(r: Row, key: IssueSortKey): string | number | null {
  switch (key) {
    case 'severity': return rowSeverity(r) === 'bad' ? 0 : 1; // criticals first when ascending
    case 'device': return r.primary.fqdn ? rowDeviceLabel(r) : null; // network-level (no fqdn) sorts last
    case 'affected': return rowAffectedLabel(r);
    case 'issue': return r.primary.title;
    case 'age': return rowFirstSeen(r); // ascending = oldest onset first
  }
}

// One-line "what this means / how to triage" per issue kind. Keeps the raw
// signal rows below it self-explanatory for an on-call admin.
function explainKind(kind: string): string | null {
  switch (kind) {
    case 'stp-root-mismatch':
      return 'This device names a different root bridge than the one jaspy elected for the VLAN. Compare the two bridge IDs below — the lower one is the root STP would actually choose.';
    case 'stp-unmonitored-root':
      return "The VLAN's real root bridge (the lowest bridge ID) is a switch jaspy does not poll. This device correctly points at it — the finding is that the true root is off-fleet, not that this device is misconfigured. Add the upstream bridge to monitoring to resolve it.";
    case 'stp-orphan':
      return 'This device has a root port (an upstream toward the root) but jaspy could not resolve the neighbour on it — usually an unmonitored switch or a missing discovered link.';
    case 'device-down':
      return 'The device stopped answering polls. Check power, the management link, and SNMP reachability.';
    case 'poe-budget':
      return 'A PoE power supply is running near its budget. New powered devices on this switch may fail to power up.';
    case 'poe-pse-down':
      return 'A PoE power supply is not operational — ports it feeds cannot deliver power.';
    case 'lag:member-link-down':
      return 'One member of this LACP uplink bundle is physically down while another is still up — the uplink keeps working but has lost its redundancy. The verdict below names the most likely loose cable end.';
    case 'iface-flapping':
      return 'This port keeps going down and back up. If it connects to another switch (see the far end below), a flapping inter-switch link is usually a bad cable, a dirty/failing SFP, or a duplex/speed mismatch — check both ends.';
    case 'lag:speed-mismatch':
      return 'Members of this port-channel are up but running at different link speeds — usually a faulty cable or a duplex/auto-negotiation fault forcing one leg to a lower speed. The bundle still forms, but throughput is capped and traffic hashes unevenly. Check the slow member’s cabling and port settings.';
  }
  if (kind.startsWith('stp-flag:multiple-roots'))
    return 'Two or more bridges each believe they are this VLAN’s root, so spanning tree has not converged to a single tree. Whether that is dangerous depends on whether they share a path — the claimed roots and their connectivity are below.';
  if (kind.startsWith('stp-flag:no-root'))
    return 'The VLAN has spanning-tree nodes but no elected root bridge — an incomplete or partitioned view.';
  if (kind.startsWith('stp-flag:cycle'))
    return 'The computed spanning tree contains a cycle — a physical or logical loop STP has not resolved.';
  if (kind.startsWith('stp-flag:multiple-root-ports'))
    return 'A device has more than one root port on this VLAN — an ambiguous path to the root.';
  if (kind.startsWith('iface-'))
    return 'An interface-health signal crossed its threshold. The counters below are over the stated window.';
  if (kind.startsWith('lag:'))
    return 'A link-aggregation (port-channel) consistency check failed. Members may not be bundling as intended.';
  return null;
}

// Render one typed detail value.
function DetailValue({ value }: { value: IssueDetailValue }) {
  switch (value.type) {
    case 'text':
      return <>{value.text}</>;
    case 'mac':
      return (
        <>
          <span className="mono">{value.mac}</span>
          {!value.monitored && <span className="badge badge-warn" style={{ marginLeft: 8 }}>off-fleet</span>}
        </>
      );
    case 'device':
      return <Link to={`/devices/${encodeURIComponent(value.fqdn)}`}>{value.hostname || value.fqdn}</Link>;
    case 'interface':
      return (
        <>
          {value.name}
          {value.state && <span className="muted"> ({value.state})</span>}
        </>
      );
    case 'verdict':
      return <span className={`verdict verdict-${value.tone}`}>{value.text}</span>;
    case 'link':
      return <Link to={value.href}>{value.text} →</Link>;
    case 'stpRoot':
      return (
        <>
          <Link to={`/devices/${encodeURIComponent(value.fqdn)}`}>{value.hostname || value.fqdn}</Link>
          {value.priority !== null && <> · priority {value.priority}</>}
          {value.mac && <> · <span className="mono">{value.mac}</span></>}
          {value.preferred && <span className="badge badge-ok" style={{ marginLeft: 8 }}>STP would elect this</span>}
        </>
      );
    case 'member':
      return (
        <>
          <span className="mono">{value.name}</span>
          {value.state && (
            <span className={`badge badge-${value.state === 'up' ? 'ok' : 'bad'}`} style={{ marginLeft: 8 }}>
              {value.state}
            </span>
          )}
          {value.speed !== null && <span className="muted"> · {value.speed} Mb/s</span>}
          {value.media && <span className="muted"> · {value.media}</span>}
          {value.peer ? (
            <>
              {' · ↔ '}
              <Link to={`/devices/${encodeURIComponent(value.peer.fqdn)}`}>
                {value.peer.fqdn}
                {value.peer.interface ? `:${value.peer.interface}` : ''}
              </Link>
            </>
          ) : value.cdp ? (
            <span className="muted">
              {' · ↔ '}
              {value.cdp.deviceId}
              {value.cdp.devicePort ? `:${value.cdp.devicePort}` : ''} (CDP)
            </span>
          ) : null}
        </>
      );
  }
}

// The expanded body: a triage explainer, description, every known signal
// (typed), any sibling issues on the same subject, timing, and (if acked)
// who/when. Shared by the mobile card and the desktop detail row.
function IssueDetail({
  issue,
  related,
  onOpenRelated,
  showExplain = true,
}: {
  issue: Issue;
  related: Issue[];
  onOpenRelated: (key: string) => void;
  // A combined row hoists the (shared) kind explainer above both ends, so the
  // per-end detail suppresses it to avoid repeating the same paragraph twice.
  showExplain?: boolean;
}) {
  const explain = showExplain ? explainKind(issue.kind) : null;
  return (
    <>
      {explain && <p className="issue-explain">{explain}</p>}
      <p style={{ margin: '4px 0 8px' }}>{issue.description}</p>
      <dl className="iface-detail">
        {issue.fqdn && (
          <>
            <dt>device</dt>
            <dd>
              <Link to={`/devices/${encodeURIComponent(issue.fqdn)}`}>{issue.fqdn}</Link>
            </dd>
          </>
        )}
        {issue.subjectLabel && (
          <>
            <dt>affected</dt>
            <dd>{issue.subjectLabel}</dd>
          </>
        )}
        {issue.detail.map((row, i) => (
          <Fragment key={`${row.label}-${i}`}>
            <dt>{row.label}</dt>
            <dd>
              <DetailValue value={row.value} />
            </dd>
          </Fragment>
        ))}
        {related.length > 0 && (
          <>
            <dt>related</dt>
            <dd className="issue-related">
              {related.map((r) => (
                <button
                  key={r.issueKey}
                  className="related-chip"
                  onClick={(e) => {
                    e.stopPropagation();
                    onOpenRelated(r.issueKey);
                  }}
                >
                  <HealthBadge severity={r.severity} label="⚠" />
                  {r.title}
                </button>
              ))}
            </dd>
          </>
        )}
        <dt>first seen</dt>
        <dd>{absolute(issue.firstSeen)} ({ago(issue.firstSeen)})</dd>
        <dt>last seen</dt>
        <dd>{ago(issue.lastSeen)}</dd>
        {issue.acknowledged && issue.ackedAt !== null && (
          <>
            <dt>acknowledged</dt>
            <dd>{absolute(issue.ackedAt)}</dd>
          </>
        )}
        {issue.acknowledged && issue.note && (
          <>
            <dt>note</dt>
            <dd>{issue.note}</dd>
          </>
        )}
      </dl>
    </>
  );
}

export default function Issues() {
  const queryClient = useQueryClient();
  const issues = useQuery({ queryKey: ['issues'], queryFn: api.issues, refetchInterval: 10000 });
  const issueTypes = useQuery({ queryKey: ['issue-types'], queryFn: api.issueTypes, refetchInterval: 30000 });

  // The "Issue types" panel: manage suppression and filter the list to one type.
  const [typesOpen, setTypesOpen] = useState(false);
  const [selectedKind, setSelectedKind] = useState<string | null>(null);
  const invalidateTypes = () => {
    queryClient.invalidateQueries({ queryKey: ['issue-types'] });
    queryClient.invalidateQueries({ queryKey: ['issues'] });
  };
  const suppress = useMutation({ mutationFn: api.suppressIssueType, onSuccess: invalidateTypes });
  const unsuppress = useMutation({ mutationFn: api.unsuppressIssueType, onSuccess: invalidateTypes });
  const onToggleSuppress = (t: IssueType) => {
    if (t.suppressed) {
      unsuppress.mutate({ kind: t.kind });
    } else {
      if (selectedKind === t.kind) setSelectedKind(null); // a suppressed type has nothing to filter
      suppress.mutate({ kind: t.kind });
    }
  };

  // Active issue count per kind (from the full, unfiltered list) for the panel.
  // A suppressed type never appears in the list, so it shows "—".
  const countByKind = useMemo(() => {
    const m = new Map<string, number>();
    for (const i of issues.data?.issues ?? []) m.set(i.kind, (m.get(i.kind) ?? 0) + 1);
    return m;
  }, [issues.data]);

  const typesByCategory = useMemo(() => {
    const groups = new Map<string, IssueType[]>();
    for (const t of issueTypes.data ?? []) {
      if (!groups.has(t.category)) groups.set(t.category, []);
      groups.get(t.category)!.push(t);
    }
    return groups;
  }, [issueTypes.data]);
  const suppressedCount = (issueTypes.data ?? []).filter((t) => t.suppressed).length;

  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const toggle = (key: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });

  // issueKey → the row key that renders it (its groupKey when combined, else
  // the issueKey). Lets a related-chip resolve which (possibly combined) row to
  // open and scroll to, since the DOM id and expanded-set are keyed by row.
  const rowKeyOf = useMemo(() => {
    const m = new Map<string, string>();
    for (const i of issues.data?.issues ?? []) m.set(i.issueKey, i.groupKey ?? i.issueKey);
    return m;
  }, [issues.data]);

  // Open a sibling issue and bring it into view — lets the related-chips on one
  // issue jump to the correlated one (e.g. the orphan behind a root mismatch).
  const openIssue = (issueKey: string) => {
    const key = rowKeyOf.get(issueKey) ?? issueKey;
    setExpanded((prev) => new Set(prev).add(key));
    requestAnimationFrame(() =>
      document.getElementById(`issue-${key}`)?.scrollIntoView({ block: 'center', behavior: 'smooth' })
    );
  };

  // Sibling issues share a device and a subject (e.g. the same VLAN) but a
  // different kind — the root-mismatch and orphan that describe one incident.
  const relatedByKey = useMemo(() => {
    const all = issues.data?.issues ?? [];
    const map = new Map<string, Issue[]>();
    for (const issue of all) {
      if (!issue.fqdn) continue;
      const subject = subjectOf(issue.issueKey);
      const siblings = all.filter(
        (o) => o.issueKey !== issue.issueKey && o.fqdn === issue.fqdn && subjectOf(o.issueKey) === subject
      );
      if (siblings.length > 0) map.set(issue.issueKey, siblings);
    }
    return map;
  }, [issues.data]);

  const invalidate = () => queryClient.invalidateQueries({ queryKey: ['issues'] });
  // The issue whose Acknowledge form is currently open, plus its in-progress
  // note text. Clicking Acknowledge opens the form rather than acking straight
  // away, so an admin can record why a known condition is expected. The form
  // opens in the full-width detail panel below the row (not in the action
  // cell), so the Acknowledge button itself never moves.
  const [ackingKey, setAckingKey] = useState<string | null>(null);
  const [ackNote, setAckNote] = useState('');
  const closeAckForm = () => {
    setAckingKey(null);
    setAckNote('');
  };
  const openAckForm = (key: string) => {
    setAckNote('');
    setAckingKey(key);
    requestAnimationFrame(() =>
      document.getElementById(`issue-${key}`)?.scrollIntoView({ block: 'center', behavior: 'smooth' })
    );
  };
  // A combined row acks both ends together: one Confirm fans out an ack per
  // member key with the same note (the backend has no batch endpoint). A single
  // row just acks its one key. Un-acknowledge fans out the same way.
  const ack = useMutation({
    mutationFn: async ({ keys, note }: { keys: string[]; note: string | null }) => {
      const results = await Promise.allSettled(keys.map((k) => api.ackIssue({ issueKey: k, note })));
      const failed = results.filter((r) => r.status === 'rejected').length;
      if (failed) throw new Error(`${failed} of ${keys.length} could not be acknowledged`);
    },
    onSuccess: () => {
      closeAckForm();
      invalidate();
    },
  });
  const unack = useMutation({
    mutationFn: async (keys: string[]) => {
      const results = await Promise.allSettled(keys.map((k) => api.unackIssue({ issueKey: k })));
      const failed = results.filter((r) => r.status === 'rejected').length;
      if (failed) throw new Error(`${failed} of ${keys.length} could not be un-acknowledged`);
    },
    onSuccess: invalidate,
  });

  // Mass-acknowledge: a selection mode over the Active list. The backend has no
  // batch endpoint, so acking many issues fans out one api.ackIssue call each.
  const [selecting, setSelecting] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const exitSelect = () => {
    setSelecting(false);
    setSelected(new Set());
  };
  const massAck = useMutation({
    mutationFn: async (keys: string[]) => {
      const results = await Promise.allSettled(
        keys.map((k) => api.ackIssue({ issueKey: k, note: null }))
      );
      const failed = results.filter((r) => r.status === 'rejected').length;
      if (failed) throw new Error(`${failed} of ${keys.length} could not be acknowledged`);
    },
    onSuccess: () => {
      exitSelect();
      invalidate();
    },
  });
  const pending = ack.isPending || unack.isPending || massAck.isPending;

  // Column sort. Default: severity (criticals first). A stable sort keeps the
  // API's first_seen-desc order within a column, so equal-severity rows read
  // newest-first.
  const [sortKey, setSortKey] = useState<IssueSortKey>('severity');
  const [sortAsc, setSortAsc] = useState(true);
  const onSort = (key: IssueSortKey) => {
    if (key === sortKey) setSortAsc(!sortAsc);
    else {
      setSortKey(key);
      setSortAsc(true);
    }
  };
  const arrow = (key: IssueSortKey) => (sortKey === key ? (sortAsc ? ' ▲' : ' ▼') : '');

  // Build rows first (combining the two ends of a link), then split into
  // Active / Acknowledged. A combined row counts as acknowledged only when
  // both ends are acked; a partially-acked group stays Active.
  const { active, acknowledged } = useMemo(() => {
    const cmp = makeCmp((r: Row) => rowSortVal(r, sortKey), sortAsc);
    const rows = buildRows((issues.data?.issues ?? []).filter((i) => !selectedKind || i.kind === selectedKind));
    return {
      active: rows.filter((r) => !rowAcked(r)).sort(cmp),
      acknowledged: rows.filter((r) => rowAcked(r)).sort(cmp),
    };
  }, [issues.data, selectedKind, sortKey, sortAsc]);

  // Selection tracks issue keys (so mass-ack fans out per end). A row is
  // selected when all its member keys are; toggling a row flips all of them.
  const rowSelected = (r: Row) => rowKeys(r).every((k) => selected.has(k));
  const toggleSelectedRow = (r: Row) => {
    const keys = rowKeys(r);
    setSelected((prev) => {
      const next = new Set(prev);
      if (keys.every((k) => next.has(k))) keys.forEach((k) => next.delete(k));
      else keys.forEach((k) => next.add(k));
      return next;
    });
  };
  // Select-all reflects the current Active list; toggling clears or fills it.
  const allSelected = active.length > 0 && active.every(rowSelected);
  const toggleSelectAll = () =>
    setSelected(allSelected ? new Set() : new Set(active.flatMap(rowKeys)));

  // The action for one issue's row/card. During mass-acknowledge selection the
  // per-row actions are hidden — the selection bar owns the acking.
  const actionButton = (row: Row) => {
    if (selecting) return null;
    if (rowAcked(row))
      return (
        <button
          className="secondary"
          disabled={pending}
          onClick={(e) => {
            e.stopPropagation();
            unack.mutate(rowKeys(row));
          }}
        >
          Un-acknowledge
        </button>
      );

    // While the reason form is open the same button becomes the confirm action,
    // so it stays put and there is no second Acknowledge button in the panel.
    if (ackingKey === row.key)
      return (
        <button
          disabled={pending}
          onClick={(e) => {
            e.stopPropagation();
            ack.mutate({ keys: rowKeys(row), note: ackNote.trim() || null });
          }}
        >
          {ack.isPending ? 'Confirming…' : 'Confirm'}
        </button>
      );

    return (
      <button
        disabled={pending}
        onClick={(e) => {
          e.stopPropagation();
          openAckForm(row.key);
        }}
      >
        Acknowledge
      </button>
    );
  };

  // The reason field, shown inside the full-width detail panel below the issue.
  // The Confirm action lives in the row's action cell (see actionButton); here
  // we only render the textarea and, at its right end, the Cancel button.
  const ackForm = () => (
    <div className="ack-form" onClick={(e) => e.stopPropagation()}>
      <textarea
        className="ack-note"
        rows={4}
        autoFocus
        placeholder="Optional reason — why is this expected? (e.g. transit links intentionally left unconnected while the event is inactive)"
        value={ackNote}
        onChange={(e) => setAckNote(e.target.value)}
      />
      <button type="button" className="secondary" disabled={pending} onClick={closeAckForm}>
        Cancel
      </button>
    </div>
  );

  // The expanded body for a row. A single issue renders its detail as before; a
  // combined link fault hoists the shared kind explainer, then stacks each end
  // under a "hostname · port" sub-heading so both sides read at a glance.
  const rowDetail = (row: Row) => {
    if (row.members.length === 1) {
      const m = row.primary;
      return <IssueDetail issue={m} related={relatedByKey.get(m.issueKey) ?? []} onOpenRelated={openIssue} />;
    }
    const explain = explainKind(row.primary.kind);
    return (
      <div className="issue-group-detail">
        {explain && <p className="issue-explain">{explain}</p>}
        {row.members.map((m) => (
          <div key={m.issueKey} className="issue-side">
            <h4 className="issue-side-head">
              <Link to={`/devices/${encodeURIComponent(m.fqdn)}`}>{m.hostname || m.fqdn}</Link>
              {m.subjectLabel && <span className="muted"> · {m.subjectLabel}</span>}
            </h4>
            <IssueDetail
              issue={m}
              related={relatedByKey.get(m.issueKey) ?? []}
              onOpenRelated={openIssue}
              showExplain={false}
            />
          </div>
        ))}
      </div>
    );
  };

  // Mobile: a card per row (a single issue, or a combined link fault) that
  // expands inline.
  const cardList = (list: Row[], selectable: boolean) => (
    <div className="item-list mobile-only">
      {list.map((row) => {
        const affected = rowAffectedLabel(row);
        return (
        <div key={row.key} id={`issue-${row.key}`} className="item-card">
          <div className="item-title">
            <button className="detail-toggle" onClick={() => toggle(row.key)} aria-expanded={expanded.has(row.key)}>
              {row.primary.title} {expanded.has(row.key) ? '▾' : '▸'}
            </button>
            {partialAckHint(row)}
            <HealthBadge severity={rowSeverity(row)} label={rowSeverity(row) === 'bad' ? '⚠ critical' : '⚠ warning'} />
            {selectable && (
              <input
                type="checkbox"
                className="select-box"
                checked={rowSelected(row)}
                onChange={() => toggleSelectedRow(row)}
                aria-label={`Select ${row.primary.title}`}
              />
            )}
          </div>
          <div className="item-sub">
            <span>{rowDeviceCell(row)}</span>
            {affected && <span>{affected}</span>}
            <span className="muted">{ago(rowFirstSeen(row))}</span>
          </div>
          {!isCombined(row) && row.primary.acknowledged && row.primary.note && (
            <div className="issue-note-inline">{row.primary.note}</div>
          )}
          {expanded.has(row.key) && rowDetail(row)}
          {ackingKey === row.key && ackForm()}
          {!selecting && <div className="actions">{actionButton(row)}</div>}
        </div>
        );
      })}
    </div>
  );

  // Desktop: a table whose rows expand into a full-width detail row.
  // In selection mode the trailing action cell holds the checkbox instead of
  // the button, so the row's columns stay exactly where they are.
  const table = (list: Row[], selectable: boolean) => (
      <div className="table-wrap desktop-only">
        <table>
          <thead>
            <tr>
              <th className="sortable" onClick={() => onSort('severity')}>Severity{arrow('severity')}</th>
              <th className="sortable" onClick={() => onSort('device')}>Device{arrow('device')}</th>
              <th className="sortable" onClick={() => onSort('affected')}>Affected{arrow('affected')}</th>
              <th className="sortable" onClick={() => onSort('issue')}>Issue{arrow('issue')}</th>
              <th className="sortable" onClick={() => onSort('age')}>Age{arrow('age')}</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {list.map((row) => {
              const sev = rowSeverity(row);
              const first = rowFirstSeen(row);
              return (
              <Fragment key={row.key}>
                <tr id={`issue-${row.key}`}>
                  <td>
                    <HealthBadge severity={sev} label={sev === 'bad' ? '⚠ critical' : '⚠ warning'} />
                  </td>
                  <td>{rowDeviceCell(row)}</td>
                  <td className="wrap-mobile">{rowAffectedLabel(row) ?? <span className="muted">—</span>}</td>
                  <td>
                    <button className="detail-toggle" onClick={() => toggle(row.key)} aria-expanded={expanded.has(row.key)}>
                      {row.primary.title} {expanded.has(row.key) ? '▾' : '▸'}
                    </button>
                    {partialAckHint(row)}
                    {!isCombined(row) && row.primary.acknowledged && row.primary.note && (
                      <div className="issue-note-inline">{row.primary.note}</div>
                    )}
                  </td>
                  <td className="muted" title={absolute(first)}>{ago(first)}</td>
                  <td className="select-cell">
                    {selectable ? (
                      <input
                        type="checkbox"
                        className="select-box"
                        checked={rowSelected(row)}
                        onChange={() => toggleSelectedRow(row)}
                        aria-label={`Select ${row.primary.title}`}
                      />
                    ) : (
                      actionButton(row)
                    )}
                  </td>
                </tr>
                {(expanded.has(row.key) || ackingKey === row.key) && (
                  <tr className="vlan-detail-row">
                    <td colSpan={6}>
                      {ackingKey === row.key && ackForm()}
                      {expanded.has(row.key) && rowDetail(row)}
                    </td>
                  </tr>
                )}
              </Fragment>
              );
            })}
          </tbody>
        </table>
      </div>
    );

  const section = (list: Row[], emptyText: string, selectable = false) =>
    list.length === 0 ? <p className="muted">{emptyText}</p> : (
      <>
        {cardList(list, selectable)}
        {table(list, selectable)}
      </>
    );

  return (
    <>
      {/* The manage/filter toggle rides on the same line as the title (right
          side), so it costs no vertical space until expanded. */}
      <div className="section-head">
        <h1>Issues</h1>
        <button
          className="section-toggle"
          onClick={() => setTypesOpen((o) => !o)}
          aria-expanded={typesOpen}
        >
          Issue types (manage / filter){' '}
          {suppressedCount > 0 && <span className="badge badge-muted">{suppressedCount} suppressed</span>}{' '}
          {typesOpen ? '▾' : '▸'}
        </button>
      </div>
      {typesOpen && (
        <div className="panel issue-types-panel">
          <div className="issue-types">
            {issueTypes.isLoading && <p className="muted">Loading types…</p>}
            {CATEGORY_ORDER.map((cat) => {
              const list = typesByCategory.get(cat);
              if (!list || list.length === 0) return null;
              return (
                <div key={cat} className="issue-type-group">
                  <h3 className="issue-type-cat">{CATEGORY_LABEL[cat]}</h3>
                  {list.map((t) => {
                    const isFilter = selectedKind === t.kind;
                    const count = countByKind.get(t.kind) ?? 0;
                    return (
                      <div key={t.kind} className={`issue-type-row ${t.suppressed ? 'suppressed' : ''}`}>
                        <button
                          className={`chip ${isFilter ? 'chip-on' : ''}`}
                          title={t.description}
                          disabled={t.suppressed}
                          aria-pressed={isFilter}
                          onClick={() => setSelectedKind(isFilter ? null : t.kind)}
                        >
                          {t.title}
                        </button>
                        <span className="issue-type-count muted">{t.suppressed ? '—' : count}</span>
                        <button
                          className="secondary issue-type-toggle"
                          disabled={suppress.isPending || unsuppress.isPending}
                          onClick={() => onToggleSuppress(t)}
                        >
                          {t.suppressed ? 'Suppressed — restore' : 'Suppress'}
                        </button>
                      </div>
                    );
                  })}
                </div>
              );
            })}
          </div>
        </div>
      )}
      <p className="muted">
        Every problem currently detected across the fleet — device reachability, interface health, spanning
        tree and link aggregation. Acknowledge an issue to move it to the list below; it re-appears if the
        condition clears and later recurs. Suppress an entire type from the Issue types panel (top right) to
        hide it everywhere.
      </p>
      {issues.isError && <p className="error">Failed to load issues: {String(issues.error)}</p>}
      {(ack.isError || unack.isError || massAck.isError || suppress.isError || unsuppress.isError) && (
        <p className="error">{String(ack.error ?? unack.error ?? massAck.error ?? suppress.error ?? unsuppress.error)}</p>
      )}

      {/* Desktop sorts via the column headers; mobile hides them, so offer sort here. */}
      <div className="toolbar mobile-only">
        <select
          aria-label="Sort issues"
          value={`${sortKey}:${sortAsc ? 'asc' : 'desc'}`}
          onChange={(e) => {
            const [key, dir] = e.target.value.split(':');
            setSortKey(key as IssueSortKey);
            setSortAsc(dir === 'asc');
          }}
        >
          <option value="severity:asc">Critical first</option>
          <option value="device:asc">Device A–Z</option>
          <option value="issue:asc">Issue A–Z</option>
          <option value="age:desc">Newest first</option>
          <option value="age:asc">Oldest first</option>
        </select>
      </div>

      {selectedKind && (
        <div className="issue-filter-active">
          <span>
            Showing only <strong>{selectedKind}</strong>
          </span>
          <button className="secondary" onClick={() => setSelectedKind(null)}>
            Clear filter
          </button>
        </div>
      )}

      <div className="section-head">
        <h2>
          Active{' '}
          {active.length > 0 && <span className="badge badge-bad">{active.length}</span>}
        </h2>
        {active.length > 0 && !selecting && (
          <button className="secondary" onClick={() => setSelecting(true)}>
            Mass acknowledge
          </button>
        )}
      </div>
      {selecting && (
        <div className="mass-ack-bar">
          <label className="mass-ack-count">
            <input
              type="checkbox"
              className="select-box"
              checked={allSelected}
              onChange={toggleSelectAll}
              aria-label="Select all active issues"
            />
            {selected.size} selected
          </label>
          <div className="mass-ack-actions">
            <button
              disabled={selected.size === 0 || massAck.isPending}
              onClick={() => massAck.mutate([...selected])}
            >
              {massAck.isPending ? 'Acknowledging…' : `Acknowledge (${selected.size}) issues`}
            </button>
            <button className="secondary" disabled={massAck.isPending} onClick={exitSelect}>
              Cancel
            </button>
          </div>
        </div>
      )}
      {section(active, issues.isLoading ? 'Loading…' : 'No active issues — the fleet is healthy. 🎉', selecting)}

      <h2 style={{ marginTop: 24 }}>
        Acknowledged{' '}
        {acknowledged.length > 0 && <span className="badge badge-muted">{acknowledged.length}</span>}
      </h2>
      {section(acknowledged, 'Nothing acknowledged.')}
    </>
  );
}
