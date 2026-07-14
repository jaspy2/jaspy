import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api, formatTimestamp, formatUptime } from '../api/client';

export default function Dashboard() {
  const queryClient = useQueryClient();
  const summary = useQuery({ queryKey: ['summary'], queryFn: api.summary, refetchInterval: 10000 });
  const discovery = useQuery({
    queryKey: ['discovery-status'],
    queryFn: api.discoveryStatus,
    refetchInterval: (query) => (query.state.data?.running ? 2000 : 10000),
  });
  const runDiscovery = useMutation({
    mutationFn: api.runDiscovery,
    onSettled: () => queryClient.invalidateQueries({ queryKey: ['discovery-status'] }),
  });

  const s = summary.data;
  const d = discovery.data;

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
              <button onClick={() => runDiscovery.mutate()} disabled={d.running || runDiscovery.isPending}>
                {d.running ? 'Discovery running…' : 'Run discovery now'}
              </button>
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
