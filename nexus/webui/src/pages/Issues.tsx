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

function issueSortVal(i: Issue, key: IssueSortKey): string | number | null {
  switch (key) {
    case 'severity': return i.severity === 'bad' ? 0 : 1; // criticals first when ascending
    case 'device': return i.fqdn ? i.hostname || i.fqdn : null; // network-level (no fqdn) sorts last
    case 'affected': return i.subjectLabel ?? null;
    case 'issue': return i.title;
    case 'age': return i.firstSeen; // ascending = oldest onset first
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
}: {
  issue: Issue;
  related: Issue[];
  onOpenRelated: (key: string) => void;
}) {
  const explain = explainKind(issue.kind);
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

  // Open a sibling issue and bring it into view — lets the related-chips on one
  // issue jump to the correlated one (e.g. the orphan behind a root mismatch).
  const openIssue = (key: string) => {
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
  const ack = useMutation({
    mutationFn: api.ackIssue,
    onSuccess: () => {
      closeAckForm();
      invalidate();
    },
  });
  const unack = useMutation({ mutationFn: api.unackIssue, onSuccess: invalidate });

  // Mass-acknowledge: a selection mode over the Active list. The backend has no
  // batch endpoint, so acking many issues fans out one api.ackIssue call each.
  const [selecting, setSelecting] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const exitSelect = () => {
    setSelecting(false);
    setSelected(new Set());
  };
  const toggleSelected = (key: string) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
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

  const { active, acknowledged } = useMemo(() => {
    const cmp = makeCmp((i: Issue) => issueSortVal(i, sortKey), sortAsc);
    const all = (issues.data?.issues ?? []).filter((i) => !selectedKind || i.kind === selectedKind);
    return {
      active: all.filter((i) => !i.acknowledged).sort(cmp),
      acknowledged: all.filter((i) => i.acknowledged).sort(cmp),
    };
  }, [issues.data, selectedKind, sortKey, sortAsc]);

  // Select-all reflects the current Active list; toggling clears or fills it.
  const allSelected = active.length > 0 && active.every((i) => selected.has(i.issueKey));
  const toggleSelectAll = () =>
    setSelected(allSelected ? new Set() : new Set(active.map((i) => i.issueKey)));

  // The action for one issue's row/card. During mass-acknowledge selection the
  // per-row actions are hidden — the selection bar owns the acking.
  const actionButton = (issue: Issue) => {
    if (selecting) return null;
    if (issue.acknowledged)
      return (
        <button
          className="secondary"
          disabled={pending}
          onClick={(e) => {
            e.stopPropagation();
            unack.mutate({ issueKey: issue.issueKey });
          }}
        >
          Un-acknowledge
        </button>
      );

    // While the reason form is open the same button becomes the confirm action,
    // so it stays put and there is no second Acknowledge button in the panel.
    if (ackingKey === issue.issueKey)
      return (
        <button
          disabled={pending}
          onClick={(e) => {
            e.stopPropagation();
            ack.mutate({ issueKey: issue.issueKey, note: ackNote.trim() || null });
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
          openAckForm(issue.issueKey);
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

  // Mobile: a card per issue that expands inline.
  const cardList = (list: Issue[], selectable: boolean) => (
    <div className="item-list mobile-only">
      {list.map((issue) => (
        <div key={issue.issueKey} id={`issue-${issue.issueKey}`} className="item-card">
          <div className="item-title">
            <button className="detail-toggle" onClick={() => toggle(issue.issueKey)} aria-expanded={expanded.has(issue.issueKey)}>
              {issue.title} {expanded.has(issue.issueKey) ? '▾' : '▸'}
            </button>
            <HealthBadge severity={issue.severity} label={issue.severity === 'bad' ? '⚠ critical' : '⚠ warning'} />
            {selectable && (
              <input
                type="checkbox"
                className="select-box"
                checked={selected.has(issue.issueKey)}
                onChange={() => toggleSelected(issue.issueKey)}
                aria-label={`Select ${issue.title}`}
              />
            )}
          </div>
          <div className="item-sub">
            <span>{deviceCell(issue)}</span>
            {issue.subjectLabel && <span>{issue.subjectLabel}</span>}
            <span className="muted">{ago(issue.firstSeen)}</span>
          </div>
          {issue.acknowledged && issue.note && <div className="issue-note-inline">{issue.note}</div>}
          {expanded.has(issue.issueKey) && (
            <IssueDetail issue={issue} related={relatedByKey.get(issue.issueKey) ?? []} onOpenRelated={openIssue} />
          )}
          {ackingKey === issue.issueKey && ackForm()}
          {!selecting && <div className="actions">{actionButton(issue)}</div>}
        </div>
      ))}
    </div>
  );

  // Desktop: a table whose rows expand into a full-width detail row.
  // In selection mode the trailing action cell holds the checkbox instead of
  // the button, so the row's columns stay exactly where they are.
  const table = (list: Issue[], selectable: boolean) => (
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
            {list.map((issue) => (
              <Fragment key={issue.issueKey}>
                <tr id={`issue-${issue.issueKey}`}>
                  <td>
                    <HealthBadge severity={issue.severity} label={issue.severity === 'bad' ? '⚠ critical' : '⚠ warning'} />
                  </td>
                  <td>{deviceCell(issue)}</td>
                  <td className="wrap-mobile">{issue.subjectLabel ?? <span className="muted">—</span>}</td>
                  <td>
                    <button className="detail-toggle" onClick={() => toggle(issue.issueKey)} aria-expanded={expanded.has(issue.issueKey)}>
                      {issue.title} {expanded.has(issue.issueKey) ? '▾' : '▸'}
                    </button>
                    {issue.acknowledged && issue.note && <div className="issue-note-inline">{issue.note}</div>}
                  </td>
                  <td className="muted" title={absolute(issue.firstSeen)}>{ago(issue.firstSeen)}</td>
                  <td className="select-cell">
                    {selectable ? (
                      <input
                        type="checkbox"
                        className="select-box"
                        checked={selected.has(issue.issueKey)}
                        onChange={() => toggleSelected(issue.issueKey)}
                        aria-label={`Select ${issue.title}`}
                      />
                    ) : (
                      actionButton(issue)
                    )}
                  </td>
                </tr>
                {(expanded.has(issue.issueKey) || ackingKey === issue.issueKey) && (
                  <tr className="vlan-detail-row">
                    <td colSpan={6}>
                      {ackingKey === issue.issueKey && ackForm()}
                      {expanded.has(issue.issueKey) && (
                        <IssueDetail issue={issue} related={relatedByKey.get(issue.issueKey) ?? []} onOpenRelated={openIssue} />
                      )}
                    </td>
                  </tr>
                )}
              </Fragment>
            ))}
          </tbody>
        </table>
      </div>
    );

  const section = (list: Issue[], emptyText: string, selectable = false) =>
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
