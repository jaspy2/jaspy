import { Fragment, useMemo, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { Link } from 'react-router-dom';
import { api } from '../api/client';
import type { Issue } from '../api/types';
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

// The expanded body: description, every known signal, timing, and (if acked)
// who/when. Shared by the mobile card and the desktop detail row.
function IssueDetail({ issue }: { issue: Issue }) {
  return (
    <>
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
        {issue.detail.map(([label, value]) => (
          <Fragment key={label}>
            <dt>{label}</dt>
            <dd>{value}</dd>
          </Fragment>
        ))}
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

  const invalidate = () => queryClient.invalidateQueries({ queryKey: ['issues'] });
  const ack = useMutation({ mutationFn: api.ackIssue, onSuccess: invalidate });
  const unack = useMutation({ mutationFn: api.unackIssue, onSuccess: invalidate });
  const pending = ack.isPending || unack.isPending;

  const { active, acknowledged } = useMemo(() => {
    const all = issues.data?.issues ?? [];
    return {
      active: all.filter((i) => !i.acknowledged),
      acknowledged: all.filter((i) => i.acknowledged),
    };
  }, [issues.data]);

  const actionButton = (issue: Issue) =>
    issue.acknowledged ? (
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
    ) : (
      <button
        disabled={pending}
        onClick={(e) => {
          e.stopPropagation();
          ack.mutate({ issueKey: issue.issueKey });
        }}
      >
        Acknowledge
      </button>
    );

  // Mobile: a card per issue that expands inline.
  const cardList = (list: Issue[]) => (
    <div className="item-list mobile-only">
      {list.map((issue) => (
        <div key={issue.issueKey} className="item-card">
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
          {expanded.has(issue.issueKey) && <IssueDetail issue={issue} />}
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
              <tr>
                <td>
                  <HealthBadge severity={issue.severity} label={issue.severity === 'bad' ? '⚠ critical' : '⚠ warning'} />
                </td>
                <td>{deviceCell(issue)}</td>
                <td className="wrap-mobile">{issue.subjectLabel ?? <span className="muted">—</span>}</td>
                <td>
                  <button className="detail-toggle" onClick={() => toggle(issue.issueKey)} aria-expanded={expanded.has(issue.issueKey)}>
                    {issue.title} {expanded.has(issue.issueKey) ? '▾' : '▸'}
                  </button>
                </td>
                <td className="muted" title={absolute(issue.firstSeen)}>{ago(issue.firstSeen)}</td>
                <td>{actionButton(issue)}</td>
              </tr>
              {expanded.has(issue.issueKey) && (
                <tr className="vlan-detail-row">
                  <td colSpan={6}>
                    <IssueDetail issue={issue} />
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
