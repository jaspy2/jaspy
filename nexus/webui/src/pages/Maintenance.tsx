import { useEffect, useRef, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api, formatTimestamp, formatUptime } from '../api/client';

function EnabledBadge({ enabled }: { enabled: boolean }) {
  return enabled
    ? <span className="badge badge-ok">enabled</span>
    : <span className="badge badge-muted">disabled</span>;
}

export default function Maintenance() {
  const queryClient = useQueryClient();
  const summary = useQuery({ queryKey: ['summary'], queryFn: api.summary, refetchInterval: 10000 });
  const system = useQuery({ queryKey: ['system'], queryFn: api.system, refetchInterval: 10000 });
  const perf = useQuery({ queryKey: ['systemPerf'], queryFn: api.systemPerf, refetchInterval: 3000 });
  const event = useQuery({ queryKey: ['event'], queryFn: api.event });

  // perfstats counters are lifetime totals; diff successive polls into live
  // per-second rates for the two headline throughput numbers.
  const prevPerf = useRef<{ polls: number; queries: number; t: number } | null>(null);
  const [rates, setRates] = useState<{ pollsPerSec: number; queriesPerSec: number } | null>(null);
  useEffect(() => {
    const p = perf.data;
    if (!p) return;
    const now = Date.now();
    const prev = prevPerf.current;
    if (prev) {
      const dt = (now - prev.t) / 1000;
      if (dt > 0.5) {
        setRates({
          pollsPerSec: Math.max(0, (p.devicePolls - prev.polls) / dt),
          queriesPerSec: Math.max(0, (p.snmpQueries - prev.queries) / dt),
        });
      }
    }
    prevPerf.current = { polls: p.devicePolls, queries: p.snmpQueries, t: now };
  }, [perf.data]);

  const [eventName, setEventName] = useState<string | null>(null);
  useEffect(() => {
    if (event.data && eventName === null) setEventName(event.data.name ?? '');
  }, [event.data, eventName]);

  const invalidateAll = () => queryClient.invalidateQueries();

  const saveEvent = useMutation({
    mutationFn: (name: string) => api.putEvent({ name: name.trim() || null }),
    onSuccess: invalidateAll,
  });

  const [confirmText, setConfirmText] = useState('');
  const [resetResult, setResetResult] = useState<string | null>(null);
  const reset = useMutation({
    mutationFn: api.reset,
    onSuccess: (result) => {
      setResetResult(`Reset complete: ${result.devicesDeleted} devices deleted.`);
      setConfirmText('');
      setEventName('');
      invalidateAll();
    },
  });

  const s = summary.data;
  const sys = system.data;
  const p = perf.data;

  return (
    <>
      <h1>Maintenance</h1>

      <h2>System status</h2>
      <div className="panel">
        {sys ? (
          <div className="kv">
            <span>SNMP poller</span>
            <span>
              <EnabledBadge enabled={sys.pollerEnabled} />
              {sys.pollerEnabled && ` polling every ${sys.pollLoopMsecs / 1000}s`}
            </span>
            <span>Pinger</span>
            <span>
              <EnabledBadge enabled={sys.pingerEnabled} />
              {' '}— device up/down from {sys.deviceStatusSource === 'pinger' ? 'ICMP ping' : 'SNMP poll responses'}
            </span>
            <span>Entity poller</span>
            <span>
              <EnabledBadge enabled={sys.entitypollerEnabled} />
              {sys.entitypollerEnabled &&
                ` every ${sys.entitypollerIntervalMsecs / 1000}s (sensors ${sys.entitypollerSensorsEnabled ? 'on' : 'off'}, STP ${sys.entitypollerStpEnabled ? 'on' : 'off'})`}
            </span>
            <span>VLAN poller</span>
            <span>
              <EnabledBadge enabled={sys.vlanpollerEnabled} />
              {sys.vlanpollerEnabled && ` every ${sys.vlanpollerIntervalMsecs / 1000}s`}
            </span>
            <span>Periodic discovery</span>
            <span>
              <EnabledBadge enabled={sys.discoveryPeriodicEnabled} />
              {sys.discoveryPeriodicEnabled ? ` every ${sys.discoveryIntervalSecs}s` : ' — manual runs only'}
            </span>
            <span>MQTT events</span>
            <span>
              {sys.mqttEnabled ? (
                <>
                  {sys.mqttConnected === true && <span className="badge badge-ok">connected</span>}
                  {sys.mqttConnected === false && <span className="badge badge-bad">disconnected</span>}
                  {sys.mqttConnected === null && <span className="badge badge-warn">connecting…</span>}
                  {' '}{sys.mqttBroker}
                </>
              ) : (
                <span className="badge badge-muted">disabled</span>
              )}
            </span>
            <span>snmpbot</span>
            <span className="wrap">
              {sys.snmpMode === 'snmpbot' && (
                <>
                  {sys.snmpbotConnected === true && <span className="badge badge-ok">responding</span>}
                  {sys.snmpbotConnected === false && <span className="badge badge-bad">not responding</span>}
                  {(sys.snmpbotConnected === null || sys.snmpbotConnected === undefined) && <span className="badge badge-warn">checking…</span>}
                  {' '}
                </>
              )}
              {sys.snmpbotUrl}
            </span>
            <span>Database</span>
            <span className="wrap">
              {sys.dbConnected ? (
                <span className="badge badge-ok">connected</span>
              ) : (
                <span className="badge badge-bad">unreachable</span>
              )}
              {sys.dbMigrationsPending === true && (
                <>
                  {' '}<span className="badge badge-warn">migrations pending</span>
                </>
              )}
              {' '}{sys.dbBackend} · {sys.dbUrl}
            </span>
            <span>Weathermap statics</span>
            <span className="wrap">
              {sys.weathermapDir ?? <span className="muted">directory not found — not served</span>}
            </span>
          </div>
        ) : (
          <p>Loading…</p>
        )}
      </div>

      <h2>Performance</h2>
      <div className="panel">
        <p className="muted" style={{ marginTop: 0 }}>
          Poll and query rates are live (from the last few seconds). Everything
          else is cumulative since the process started
          {s ? ` ${formatUptime(s.startupTime)} ago` : ''}: means are lifetime
          averages and maxima are all-time high-water marks, not a recent window.
        </p>
        {p ? (
          <div className="kv">
            <span>Device polls</span>
            <span>
              {rates ? `${rates.pollsPerSec.toFixed(1)}/s` : '…'}{' '}
              <span className="muted">({p.devicePolls.toLocaleString()} total)</span>
              {p.pollOverruns > 0 && (
                <>{' '}<span className="badge badge-warn">{p.pollOverruns.toLocaleString()} overruns</span></>
              )}
            </span>
            <span>SNMP queries</span>
            <span>
              {rates ? `${rates.queriesPerSec.toFixed(0)}/s` : '…'}{' '}
              {p.snmpErrors > 0
                ? <span className="badge badge-bad">{p.snmpErrorPct.toFixed(1)}% errors</span>
                : <span className="badge badge-ok">no errors</span>}
            </span>
            <span>SNMP latency</span>
            <span>mean {p.snmpMeanMs.toFixed(1)} ms · max {p.snmpMaxMs.toFixed(0)} ms</span>
            <span>Poll iteration</span>
            <span>mean {p.pollIterMeanMs.toFixed(1)} ms · max {p.pollIterMaxMs.toFixed(0)} ms</span>
            <span>SNMP in-flight</span>
            <span>{p.snmpInflight} now · {p.snmpInflightMax} peak</span>
            <span>IMDS lock wait</span>
            <span>mean {p.imdsLockWaitMeanMs.toFixed(2)} ms · max {p.imdsLockWaitMaxMs.toFixed(0)} ms</span>
            <span>IMDS report hold</span>
            <span>mean {p.imdsReportMeanMs.toFixed(2)} ms</span>
            <span>Metrics build</span>
            <span>max {p.metricsBuildMaxMs.toFixed(1)} ms · {p.metricsScrapes.toLocaleString()} scrapes</span>
            <span>Interfaces reported</span>
            <span>{p.interfacesReported.toLocaleString()}</span>
            {sys?.snmpMode === 'embedded' && (
              <>
                <span>SNMP socket opens</span>
                <span>{p.snmpSessionOpens.toLocaleString()}</span>
              </>
            )}
          </div>
        ) : (
          <p>Loading…</p>
        )}
      </div>

      <h2>Event</h2>
      <div className="panel">
        <p className="muted">
          Jaspy drives one event (e.g. a LAN party) at a time. The event name is
          shown in the header and cleared by a state reset.
        </p>
        <form
          className="form"
          onSubmit={(e) => {
            e.preventDefault();
            if (eventName !== null) saveEvent.mutate(eventName);
          }}
        >
          <label>
            Event name
            <input
              value={eventName ?? ''}
              onChange={(e) => setEventName(e.target.value)}
              placeholder="Assembly Winter 2026"
            />
          </label>
          <div className="actions">
            <button type="submit" disabled={saveEvent.isPending}>Save event</button>
            {saveEvent.isSuccess && <span className="ok">Saved.</span>}
            {saveEvent.isError && <span className="error">{String(saveEvent.error)}</span>}
          </div>
        </form>
      </div>

      <h2>Runtime</h2>
      <div className="panel">
        {s ? (
          <div className="kv">
            <span>Version</span><span>{s.version}</span>
            <span>Started</span><span>{formatTimestamp(s.startupTime)}</span>
            <span>Uptime</span><span>{formatUptime(s.startupTime)}</span>
            <span>State id</span><span>{s.stateId}</span>
            <span>Weathermap</span><span><a href="/weathermap/" target="_blank" rel="noreferrer">open ↗</a></span>
          </div>
        ) : (
          <p>Loading…</p>
        )}
      </div>

      <h2 className="danger-heading">Danger zone</h2>
      <div className="panel danger-zone">
        <p>
          Reset deletes <strong>all devices, interfaces, links, client locations and
          weathermap positions</strong> and clears the event name, so a new event can be
          set up from scratch. Discovery configuration is kept. This cannot be undone.
        </p>
        <label>
          Type <code>RESET</code> to confirm
          <input value={confirmText} onChange={(e) => setConfirmText(e.target.value)} placeholder="RESET" />
        </label>
        <div className="actions">
          <button
            className="danger"
            disabled={confirmText !== 'RESET' || reset.isPending}
            onClick={() => reset.mutate()}
          >
            Reset jaspy state
          </button>
          {resetResult && <span className="ok">{resetResult}</span>}
          {reset.isError && <span className="error">{String(reset.error)}</span>}
        </div>
      </div>
    </>
  );
}
