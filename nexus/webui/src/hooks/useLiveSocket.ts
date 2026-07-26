import { useEffect, useRef, useState } from 'react';
import { liveLogSocketUrl } from '../api/client';

// Reconnect backoff: start fast (a page load racing a server restart should
// recover in well under a second), cap low so "eventually" is never far away.
const BACKOFF_MIN_MS = 500;
const BACKOFF_MAX_MS = 5000;
// Abort a handshake stuck in CONNECTING (e.g. server mid-restart).
const CONNECT_TIMEOUT_MS = 5000;
// Wake probe: a socket can read OPEN over a TCP link that died while the device
// slept. On wake we send an app-level ping and give the server this long to
// answer "pong"; no answer means the socket is a zombie — close and reconnect.
const PROBE_TIMEOUT_MS = 3000;
// Wall-clock watchdog: fire every tick, and if far more than a tick elapsed the
// device slept or timers were throttled while backgrounded — re-verify the
// socket even though no visibility/focus event fired (e.g. a foreground laptop
// tab whose machine was asleep).
const WATCHDOG_INTERVAL_MS = 20000;
const WATCHDOG_GAP_MS = 40000;

// live      — socket open and just verified/streaming: data is up to date.
// connecting — no live socket, actively (re)connecting: data may be stale.
// down      — the OS reports no network: data is stale until it returns.
export type LiveStatus = 'live' | 'connecting' | 'down';

export interface LiveSocketOptions {
  // Called with each JSON-parsed frame (malformed frames are dropped).
  onMessage: (data: unknown) => void;
  // Called on every (re)connect, before any frames arrive — e.g. to reset
  // state that the server replays from its backlog, or to refetch data that
  // may have changed while the socket was down.
  onOpen?: () => void;
  // Bump this counter when the page just did something that needs the live
  // socket (e.g. triggered a run): a disconnected socket retries immediately
  // instead of waiting out its backoff.
  reconnectSignal?: number;
}

// Subscription to a server live-update topic (/api/v1/ws/logs/<topic>) with
// automatic reconnect: capped exponential backoff, handshake timeout, and an
// active liveness re-check on tab focus / visibility / network return / device
// wake so a woken tab never shows stale data behind a dead-but-OPEN socket.
export default function useLiveSocket(
  topic: string,
  { onMessage, onOpen, reconnectSignal = 0 }: LiveSocketOptions,
): { connected: boolean; status: LiveStatus } {
  const [status, setStatus] = useState<LiveStatus>('connecting');

  const socketRef = useRef<WebSocket | null>(null);
  const retryTimer = useRef<number | undefined>(undefined);
  const connectTimer = useRef<number | undefined>(undefined);
  const probeTimer = useRef<number | undefined>(undefined);
  const backoffMs = useRef(BACKOFF_MIN_MS);
  const disposed = useRef(false);
  const connectRef = useRef<() => void>(() => {});
  const checkRef = useRef<() => void>(() => {});

  // Keep the latest callbacks without re-running the connection effect.
  const onMessageRef = useRef(onMessage);
  onMessageRef.current = onMessage;
  const onOpenRef = useRef(onOpen);
  onOpenRef.current = onOpen;

  useEffect(() => {
    disposed.current = false;
    backoffMs.current = BACKOFF_MIN_MS;

    const clearProbe = () => {
      window.clearTimeout(probeTimer.current);
      probeTimer.current = undefined;
    };

    const connect = () => {
      if (disposed.current) return;
      const existing = socketRef.current;
      if (
        existing &&
        (existing.readyState === WebSocket.OPEN || existing.readyState === WebSocket.CONNECTING)
      ) {
        return; // already live or handshaking
      }
      window.clearTimeout(retryTimer.current);
      clearProbe();
      if (status !== 'live') setStatus('connecting');
      const socket = new WebSocket(liveLogSocketUrl(topic));
      socketRef.current = socket;
      connectTimer.current = window.setTimeout(() => {
        if (socket.readyState === WebSocket.CONNECTING) socket.close();
      }, CONNECT_TIMEOUT_MS);
      socket.onopen = () => {
        window.clearTimeout(connectTimer.current);
        backoffMs.current = BACKOFF_MIN_MS;
        setStatus('live');
        onOpenRef.current?.();
      };
      socket.onmessage = (event) => {
        let data: unknown;
        try {
          data = JSON.parse(event.data);
        } catch {
          return; // ignore malformed frames
        }
        // Our own liveness probe's reply: it confirms the socket is alive but
        // isn't application data — swallow it, don't forward.
        if (data && typeof data === 'object' && (data as { type?: unknown }).type === 'pong') {
          clearProbe();
          return;
        }
        onMessageRef.current(data);
      };
      socket.onclose = () => {
        window.clearTimeout(connectTimer.current);
        clearProbe();
        // Offline gets its own "down" state; a plain drop is a reconnect.
        setStatus(navigator.onLine === false ? 'down' : 'connecting');
        if (!disposed.current) {
          retryTimer.current = window.setTimeout(connect, backoffMs.current);
          backoffMs.current = Math.min(backoffMs.current * 2, BACKOFF_MAX_MS);
        }
      };
    };
    connectRef.current = connect;

    // Re-verify the connection on wake. Crucially, never trust readyState after
    // sleep: an OPEN socket may sit on a dead TCP link, so we actively probe it
    // (browsers can't send native WS pings from JS) and force a reconnect if no
    // pong comes back. A socket that isn't open reconnects immediately.
    const checkConnection = () => {
      if (disposed.current) return;
      const socket = socketRef.current;
      if (socket && socket.readyState === WebSocket.OPEN) {
        if (probeTimer.current !== undefined) return; // probe already in flight
        try {
          socket.send(JSON.stringify({ type: 'ping' }));
        } catch {
          backoffMs.current = BACKOFF_MIN_MS;
          socket.close(); // send failed → dead; onclose reconnects
          return;
        }
        probeTimer.current = window.setTimeout(() => {
          probeTimer.current = undefined;
          socketRef.current?.close(); // no pong → zombie; onclose reconnects
        }, PROBE_TIMEOUT_MS);
        return;
      }
      if (socket && socket.readyState === WebSocket.CONNECTING) return;
      backoffMs.current = BACKOFF_MIN_MS;
      connect();
    };
    checkRef.current = checkConnection;

    connect();

    const onWake = () => {
      if (document.visibilityState !== 'hidden') checkConnection();
    };
    const onOffline = () => {
      setStatus('down');
      socketRef.current?.close();
    };
    document.addEventListener('visibilitychange', onWake);
    window.addEventListener('focus', onWake);
    window.addEventListener('pageshow', onWake); // bfcache restore (mobile back/fwd)
    window.addEventListener('online', onWake);
    window.addEventListener('offline', onOffline);

    let lastTick = Date.now();
    const watchdog = window.setInterval(() => {
      const now = Date.now();
      const gap = now - lastTick;
      lastTick = now;
      if (gap > WATCHDOG_GAP_MS && document.visibilityState !== 'hidden') checkConnection();
    }, WATCHDOG_INTERVAL_MS);

    return () => {
      disposed.current = true;
      document.removeEventListener('visibilitychange', onWake);
      window.removeEventListener('focus', onWake);
      window.removeEventListener('pageshow', onWake);
      window.removeEventListener('online', onWake);
      window.removeEventListener('offline', onOffline);
      window.clearInterval(watchdog);
      window.clearTimeout(retryTimer.current);
      window.clearTimeout(connectTimer.current);
      clearProbe();
      socketRef.current?.close();
      socketRef.current = null;
    };
    // status is read inside connect() only to avoid a redundant setState; it is
    // intentionally not a dependency (the effect owns the socket for `topic`).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [topic]);

  // Parent-initiated nudge: skip the backoff and try now (no-op if connected).
  useEffect(() => {
    if (reconnectSignal > 0) {
      backoffMs.current = BACKOFF_MIN_MS;
      checkRef.current();
    }
  }, [reconnectSignal]);

  return { connected: status === 'live', status };
}
