import { Fragment, useMemo, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { Link } from 'react-router-dom';
import { api } from '../api/client';
import type { Issue, IssueDetailValue } from '../api/types';
import { HealthBadge } from '../components/StatusBadge';

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
  // away, so an admin can record why a known condition is expected.
  const [ackingKey, setAckingKey] = useState<string | null>(null);
  const [ackNote, setAckNote] = useState('');
  const closeAckForm = () => {
    setAckingKey(null);
    setAckNote('');
  };
  const ack = useMutation({
    mutationFn: api.ackIssue,
    onSuccess: () => {
      closeAckForm();
      invalidate();
    },
  });
  const unack = useMutation({ mutationFn: api.unackIssue, onSuccess: invalidate });
  const pending = ack.isPending || unack.isPending;

  const { active, acknowledged } = useMemo(() => {
    const all = issues.data?.issues ?? [];
    return {
      active: all.filter((i) => !i.acknowledged),
      acknowledged: all.filter((i) => i.acknowledged),
    };
  }, [issues.data]);

  const actionButton = (issue: Issue) => {
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

    if (ackingKey === issue.issueKey)
      return (
        <form
          className="ack-form"
          onClick={(e) => e.stopPropagation()}
          onSubmit={(e) => {
            e.preventDefault();
            const note = ackNote.trim();
            ack.mutate({ issueKey: issue.issueKey, note: note || null });
          }}
        >
          <textarea
            className="ack-note"
            rows={4}
            autoFocus
            placeholder="Optional reason — why is this expected? (e.g. transit links intentionally left unconnected while the event is inactive)"
            value={ackNote}
            onChange={(e) => setAckNote(e.target.value)}
          />
          <div className="ack-form-actions">
            <button type="submit" disabled={pending}>
              {pending ? 'Acknowledging…' : 'Acknowledge'}
            </button>
            <button type="button" className="secondary" disabled={pending} onClick={closeAckForm}>
              Cancel
            </button>
          </div>
        </form>
      );

    return (
      <button
        disabled={pending}
        onClick={(e) => {
          e.stopPropagation();
          setAckNote('');
          setAckingKey(issue.issueKey);
        }}
      >
        Acknowledge
      </button>
    );
  };

  // Mobile: a card per issue that expands inline.
  const cardList = (list: Issue[]) => (
    <div className="item-list mobile-only">
      {list.map((issue) => (
        <div key={issue.issueKey} id={`issue-${issue.issueKey}`} className="item-card">
          <div className="item-title">
            <button className="detail-toggle" onClick={() => toggle(issue.issueKey)} aria-expanded={expanded.has(issue.issueKey)}>
              {issue.title} {expanded.has(issue.issueKey) ? '▾' : '▸'}
            </button>
            <HealthBadge severity={issue.severity} label={issue.severity === 'bad' ? '⚠ critical' : '⚠ warning'} />
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
          <div className="actions">{actionButton(issue)}</div>
        </div>
      ))}
    </div>
  );

  // Desktop: a table whose rows expand into a full-width detail row.
  const table = (list: Issue[]) => (
    <div className="table-wrap desktop-only">
      <table>
        <thead>
          <tr>
            <th>Severity</th>
            <th>Device</th>
            <th>Affected</th>
            <th>Issue</th>
            <th>Age</th>
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
                <td>{actionButton(issue)}</td>
              </tr>
              {expanded.has(issue.issueKey) && (
                <tr className="vlan-detail-row">
                  <td colSpan={6}>
                    <IssueDetail issue={issue} related={relatedByKey.get(issue.issueKey) ?? []} onOpenRelated={openIssue} />
                  </td>
                </tr>
              )}
            </Fragment>
          ))}
        </tbody>
      </table>
    </div>
  );

  const section = (list: Issue[], emptyText: string) =>
    list.length === 0 ? <p className="muted">{emptyText}</p> : (
      <>
        {cardList(list)}
        {table(list)}
      </>
    );

  return (
    <>
      <h1>Issues</h1>
      <p className="muted">
        Every problem currently detected across the fleet — device reachability, interface health, spanning
        tree and link aggregation. Acknowledge an issue to move it to the list below; it re-appears if the
        condition clears and later recurs.
      </p>
      {issues.isError && <p className="error">Failed to load issues: {String(issues.error)}</p>}
      {(ack.isError || unack.isError) && (
        <p className="error">{String(ack.error ?? unack.error)}</p>
      )}

      <h2>
        Active{' '}
        {active.length > 0 && <span className="badge badge-bad">{active.length}</span>}
      </h2>
      {section(active, issues.isLoading ? 'Loading…' : 'No active issues — the fleet is healthy. 🎉')}

      <h2 style={{ marginTop: 24 }}>
        Acknowledged{' '}
        {acknowledged.length > 0 && <span className="badge badge-muted">{acknowledged.length}</span>}
      </h2>
      {section(acknowledged, 'Nothing acknowledged.')}
    </>
  );
}
