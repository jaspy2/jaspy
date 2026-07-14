import { useEffect, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api, formatTimestamp } from '../api/client';
import type { DiscoveryConfig } from '../api/types';
import LiveLog from '../components/LiveLog';

interface FormState {
  rootDevice: string;
  community: string;
  dnsDomains: string;
  ignore: string;
  remap: string;
  topologyStable: boolean;
  periodicEnabled: boolean;
  intervalSecs: number;
}

function toForm(config: DiscoveryConfig): FormState {
  return {
    rootDevice: config.rootDevice ?? '',
    community: config.community ?? '',
    dnsDomains: config.dnsDomains.join(', '),
    ignore: config.ignore.join(', '),
    remap: Object.entries(config.remap).map(([src, dst]) => `${src}:${dst}`).join(', '),
    topologyStable: config.topologyStable,
    periodicEnabled: config.periodicEnabled,
    intervalSecs: config.intervalSecs,
  };
}

function splitCsv(value: string): string[] {
  return value.split(',').map((s) => s.trim()).filter((s) => s.length > 0);
}

function fromForm(form: FormState): DiscoveryConfig {
  const remap: Record<string, string> = {};
  for (const entry of splitCsv(form.remap)) {
    const idx = entry.indexOf(':');
    if (idx > 0) remap[entry.slice(0, idx)] = entry.slice(idx + 1);
  }
  return {
    rootDevice: form.rootDevice.trim() || null,
    community: form.community.trim() || null,
    dnsDomains: splitCsv(form.dnsDomains),
    ignore: splitCsv(form.ignore),
    remap,
    topologyStable: form.topologyStable,
    periodicEnabled: form.periodicEnabled,
    intervalSecs: form.intervalSecs,
  };
}

export default function Discovery() {
  const queryClient = useQueryClient();
  const config = useQuery({ queryKey: ['discovery-config'], queryFn: api.discoveryConfig });
  const status = useQuery({
    queryKey: ['discovery-status'],
    queryFn: api.discoveryStatus,
    refetchInterval: (query) => (query.state.data?.running ? 2000 : 5000),
  });

  const [form, setForm] = useState<FormState | null>(null);
  useEffect(() => {
    if (config.data && form === null) setForm(toForm(config.data));
  }, [config.data, form]);

  const saveConfig = useMutation({
    mutationFn: (body: DiscoveryConfig) => api.putDiscoveryConfig(body),
    onSuccess: (saved) => {
      queryClient.setQueryData(['discovery-config'], saved);
      setForm(toForm(saved));
    },
  });

  // Bumped when a run is triggered so the log panel retries a dropped
  // websocket immediately instead of waiting out its reconnect backoff.
  const [logNudge, setLogNudge] = useState(0);
  const runDiscovery = useMutation({
    mutationFn: api.runDiscovery,
    onMutate: () => setLogNudge((n) => n + 1),
    onSettled: () => queryClient.invalidateQueries({ queryKey: ['discovery-status'] }),
  });

  const set = <K extends keyof FormState>(key: K, value: FormState[K]) =>
    setForm((f) => (f ? { ...f, [key]: value } : f));

  const s = status.data;
  const configReady = Boolean(config.data?.rootDevice && config.data?.community);

  return (
    <>
      <h1>Discovery</h1>

      <h2>Status</h2>
      <div className="panel">
        {s ? (
          <>
            <div className="kv">
              <span>Status</span>
              <span>{s.running ? <span className="badge badge-warn">running…</span> : <span className="badge badge-muted">idle</span>}</span>
              <span>Last started</span><span>{formatTimestamp(s.lastStarted)}</span>
              <span>Last finished</span><span>{formatTimestamp(s.lastFinished)}</span>
              <span>Devices found</span><span>{s.devicesFound ?? '—'}</span>
              <span>Devices failed</span>
              <span>{s.devicesFailed ? <span className="error">{s.devicesFailed}</span> : s.devicesFailed ?? '—'}</span>
              <span>Links found</span><span>{s.linksFound ?? '—'}</span>
              {s.lastError && (
                <>
                  <span>Last error</span><span className="error">{s.lastError}</span>
                </>
              )}
            </div>
            <div className="actions">
              <button
                onClick={() => runDiscovery.mutate()}
                disabled={s.running || runDiscovery.isPending || !configReady}
                title={configReady ? undefined : 'Set a root device and SNMP community in the configuration below first'}
              >
                {s.running ? 'Discovery running…' : 'Run discovery now'}
              </button>
              {!configReady && (
                <span className="muted">Set a root device and SNMP community in the configuration below to enable runs.</span>
              )}
              {runDiscovery.isError && <span className="error">{String(runDiscovery.error)}</span>}
            </div>
          </>
        ) : (
          <p>Loading…</p>
        )}
      </div>

      <h2>Log</h2>
      <div className="panel">
        <LiveLog topic="discovery" reconnectSignal={logNudge} />
      </div>

      <h2>Configuration</h2>
      <div className="panel">
        {form ? (
          <form
            className="form"
            onSubmit={(e) => {
              e.preventDefault();
              saveConfig.mutate(fromForm(form));
            }}
          >
            <label>
              Root device
              <input value={form.rootDevice} onChange={(e) => set('rootDevice', e.target.value)} placeholder="core-sw.event.example" />
            </label>
            <label>
              SNMP community
              <input value={form.community} onChange={(e) => set('community', e.target.value)} placeholder="public" />
            </label>
            <label>
              DNS search domains (comma separated)
              <input value={form.dnsDomains} onChange={(e) => set('dnsDomains', e.target.value)} placeholder="event.example" />
            </label>
            <label>
              Ignored devices (comma separated fqdns)
              <input value={form.ignore} onChange={(e) => set('ignore', e.target.value)} />
            </label>
            <label>
              Name remaps (comma separated src:dst)
              <input value={form.remap} onChange={(e) => set('remap', e.target.value)} />
            </label>
            <label className="checkbox">
              <input type="checkbox" checked={form.topologyStable} onChange={(e) => set('topologyStable', e.target.checked)} />
              Topology stable (never tear down known links)
            </label>
            <label className="checkbox">
              <input type="checkbox" checked={form.periodicEnabled} onChange={(e) => set('periodicEnabled', e.target.checked)} />
              Periodic discovery
            </label>
            <label>
              Interval (seconds)
              <input
                type="number"
                min={30}
                value={form.intervalSecs}
                onChange={(e) => set('intervalSecs', Number(e.target.value))}
                disabled={!form.periodicEnabled}
              />
            </label>
            <div className="actions">
              <button type="submit" disabled={saveConfig.isPending}>Save configuration</button>
              {saveConfig.isSuccess && <span className="ok">Saved (persists across restarts).</span>}
              {saveConfig.isError && <span className="error">{String(saveConfig.error)}</span>}
            </div>
          </form>
        ) : (
          <p>Loading…</p>
        )}
      </div>
    </>
  );
}
