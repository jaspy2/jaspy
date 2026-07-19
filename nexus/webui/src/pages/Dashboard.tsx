import { Link } from 'react-router-dom';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api, formatTimestamp, formatUptime } from '../api/client';
import { HealthBadge } from '../components/StatusBadge';

// Compact "5m ago" from an epoch-ms timestamp.
function ago(ms: number): string {
  const s = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

export default function Dashboard() {
  const queryClient = useQueryClient();
  const summary = useQuery({ queryKey: ['summary'], queryFn: api.summary, refetchInterval: 10000 });
  const issues = useQuery({ queryKey: ['issues'], queryFn: api.issues, refetchInterval: 10000 });
  const discovery = useQuery({
    queryKey: ['discovery-status'],
    queryFn: api.discoveryStatus,
    refetchInterval: (query) => (query.state.data?.running ? 2000 : 10000),
  });
  const config = useQuery({ queryKey: ['discovery-config'], queryFn: api.discoveryConfig });
  const configReady = Boolean(config.data?.rootDevice && config.data?.community);
  const runDiscovery = useMutation({
    mutationFn: api.runDiscovery,
    onSettled: () => queryClient.invalidateQueries({ queryKey: ['discovery-status'] }),
  });

  const s = summary.data;
  const d = discovery.data;
  const activeIssues = (issues.data?.issues ?? []).filter((i) => !i.acknowledged);
  const recentIssues = activeIssues.slice(0, 10);

  return (
    <>
      <h1>Dashboard</h1>
      {summary.isError && <p className="error">Failed to load summary: {String(summary.error)}</p>}
      {s && (
        <div className="cards">
          <div className="card">
            <div className="card-value ok">{s.devicesUp}</div>
            <div className="card-label">devices up</div>
          </div>
          <div className="card">
            <div className="card-value bad">{s.devicesDown}</div>
            <div className="card-label">devices down</div>
          </div>
          <div className="card">
            <div className="card-value">{s.devicesUnknown}</div>
            <div className="card-label">unknown</div>
          </div>
          <div className="card">
            <div className="card-value">{s.deviceCount}</div>
            <div className="card-label">devices total</div>
          </div>
          <div className="card">
            <div className="card-value">{s.eventName ?? '—'}</div>
            <div className="card-label">current event</div>
          </div>
          <div className="card">
            <div className="card-value">{formatUptime(s.startupTime)}</div>
            <div className="card-label">nexus uptime (v{s.version})</div>
          </div>
        </div>
      )}

      <h2>
        Issues{' '}
        {activeIssues.length > 0 && <span className="badge badge-bad">{activeIssues.length}</span>}
      </h2>
      <div className="panel">
        {issues.isError && <p className="error">Failed to load issues: {String(issues.error)}</p>}
        {recentIssues.length === 0 ? (
          <p className="muted">{issues.isLoading ? 'Loading…' : 'No active issues — the fleet is healthy. 🎉'}</p>
        ) : (
          <div className="item-list">
            {recentIssues.map((issue) => (
              <div key={issue.issueKey} className="item-card">
                <div className="item-title">
                  <span>
                    {issue.fqdn ? (
                      <Link to={`/devices/${encodeURIComponent(issue.fqdn)}`}>{issue.hostname || issue.fqdn}</Link>
                    ) : (
                      <span className="muted">network</span>
                    )}{' '}
                    — {issue.title}
                  </span>
                  <HealthBadge severity={issue.severity} label={issue.severity === 'bad' ? '⚠ critical' : '⚠ warning'} />
                </div>
                <div className="item-sub">
                  {issue.subjectLabel && <span>{issue.subjectLabel}</span>}
                  <span className="muted">{ago(issue.firstSeen)}</span>
                </div>
              </div>
            ))}
          </div>
        )}
        <div className="actions">
          <Link to="/issues">View all issues →</Link>
        </div>
      </div>

      <h2>Discovery</h2>
      <div className="panel">
        {d ? (
          <>
            <div className="kv">
              <span>Status</span>
              <span>{d.running ? <span className="badge badge-warn">running…</span> : <span className="badge badge-muted">idle</span>}</span>
              <span>Last started</span>
              <span>{formatTimestamp(d.lastStarted)}</span>
              <span>Last finished</span>
              <span>{formatTimestamp(d.lastFinished)}</span>
              <span>Devices found</span>
              <span>{d.devicesFound ?? '—'}</span>
              <span>Links found</span>
              <span>{d.linksFound ?? '—'}</span>
              {d.lastError && (
                <>
                  <span>Last error</span>
                  <span className="error">{d.lastError}</span>
                </>
              )}
            </div>
            <div className="actions">
              <button
                onClick={() => runDiscovery.mutate()}
                disabled={d.running || runDiscovery.isPending || !configReady}
                title={configReady ? undefined : 'Discovery is not configured yet'}
              >
                {d.running ? 'Discovery running…' : 'Run discovery now'}
              </button>
              {!configReady && (
                <span className="muted">
                  Discovery needs a root device and SNMP community — <Link to="/discovery">configure it here</Link>.
                </span>
              )}
              {runDiscovery.isError && <span className="error">{String(runDiscovery.error)}</span>}
            </div>
          </>
        ) : (
          <p>Loading…</p>
        )}
      </div>
    </>
  );
}
