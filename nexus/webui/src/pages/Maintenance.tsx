import { useEffect, useState } from 'react';
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
  const event = useQuery({ queryKey: ['event'], queryFn: api.event });

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
            <span>snmpbot</span><span className="wrap">{sys.snmpbotUrl}</span>
            <span>Database</span><span className="wrap">{sys.dbUrl}</span>
            <span>Weathermap statics</span>
            <span className="wrap">
              {sys.weathermapDir ?? <span className="muted">directory not found — not served</span>}
            </span>
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
