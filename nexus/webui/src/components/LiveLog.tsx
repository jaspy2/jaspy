import { useEffect, useRef, useState } from 'react';
import { liveLogSocketUrl } from '../api/client';
import type { LiveLogLine } from '../api/types';

// Keep the view bounded; the server backlog is capped too (500 lines).
const MAX_LINES = 1000;
// Reconnect backoff: start fast (a page load racing a server restart should
// recover in well under a second), cap low so "eventually" is never far away.
const BACKOFF_MIN_MS = 500;
const BACKOFF_MAX_MS = 5000;
// Abort a handshake stuck in CONNECTING (e.g. server mid-restart).
const CONNECT_TIMEOUT_MS = 5000;

interface Props {
  topic: string;
  // Bump this counter when the page just did something that needs the live
  // log (e.g. triggered a run): a disconnected socket retries immediately
  // instead of waiting out its backoff.
  reconnectSignal?: number;
}

// Scrolling tail of a server-side live log topic. The server replays its
// backlog on connect, then pushes new lines; the socket reconnects on loss.
export default function LiveLog({ topic, reconnectSignal = 0 }: Props) {
  const [lines, setLines] = useState<LiveLogLine[]>([]);
  const [connected, setConnected] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  const followTail = useRef(true);

  const socketRef = useRef<WebSocket | null>(null);
  const retryTimer = useRef<number | undefined>(undefined);
  const connectTimer = useRef<number | undefined>(undefined);
  const backoffMs = useRef(BACKOFF_MIN_MS);
  const disposed = useRef(false);
  const connectRef = useRef<() => void>(() => {});

  useEffect(() => {
    disposed.current = false;
    backoffMs.current = BACKOFF_MIN_MS;

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
      const socket = new WebSocket(liveLogSocketUrl(topic));
      socketRef.current = socket;
      connectTimer.current = window.setTimeout(() => {
        if (socket.readyState === WebSocket.CONNECTING) socket.close();
      }, CONNECT_TIMEOUT_MS);
      socket.onopen = () => {
        window.clearTimeout(connectTimer.current);
        backoffMs.current = BACKOFF_MIN_MS;
        setConnected(true);
        // The server replays its backlog on every connect; start clean so a
        // reconnect does not duplicate history.
        setLines([]);
      };
      socket.onmessage = (event) => {
        try {
          const entry = JSON.parse(event.data) as LiveLogLine;
          setLines((prev) => [...prev, entry].slice(-MAX_LINES));
        } catch {
          /* ignore malformed frames */
        }
      };
      socket.onclose = () => {
        window.clearTimeout(connectTimer.current);
        setConnected(false);
        if (!disposed.current) {
          retryTimer.current = window.setTimeout(connect, backoffMs.current);
          backoffMs.current = Math.min(backoffMs.current * 2, BACKOFF_MAX_MS);
        }
      };
    };
    connectRef.current = connect;
    connect();

    // Retry immediately when the tab wakes up or the network returns, instead
    // of waiting out a backoff that grew while the page was in the background.
    const onWake = () => {
      if (document.visibilityState !== 'hidden') {
        backoffMs.current = BACKOFF_MIN_MS;
        connect();
      }
    };
    document.addEventListener('visibilitychange', onWake);
    window.addEventListener('focus', onWake);
    window.addEventListener('online', onWake);

    return () => {
      disposed.current = true;
      document.removeEventListener('visibilitychange', onWake);
      window.removeEventListener('focus', onWake);
      window.removeEventListener('online', onWake);
      window.clearTimeout(retryTimer.current);
      window.clearTimeout(connectTimer.current);
      socketRef.current?.close();
      socketRef.current = null;
    };
  }, [topic]);

  // Parent-initiated nudge: skip the backoff and try now (no-op if connected).
  useEffect(() => {
    if (reconnectSignal > 0) {
      backoffMs.current = BACKOFF_MIN_MS;
      connectRef.current();
    }
  }, [reconnectSignal]);

  // Follow the tail unless the user scrolled up to read history.
  useEffect(() => {
    const el = scrollRef.current;
    if (el && followTail.current) el.scrollTop = el.scrollHeight;
  }, [lines]);

  const onScroll = () => {
    const el = scrollRef.current;
    if (el) followTail.current = el.scrollHeight - el.scrollTop - el.clientHeight < 10;
  };

  return (
    <div className="livelog">
      <div className="livelog-lines" ref={scrollRef} onScroll={onScroll}>
        {lines.length === 0 && (
          <span className="muted">{connected ? 'No log messages yet.' : 'Connecting…'}</span>
        )}
        {lines.map((entry, i) => (
          <div key={i} className="livelog-line">
            <span className="livelog-ts">{new Date(entry.ts * 1000).toLocaleTimeString()}</span>{' '}
            {entry.line}
          </div>
        ))}
      </div>
      <div className="livelog-status muted">
        {connected ? 'live' : 'disconnected — reconnecting…'}
      </div>
    </div>
  );
}
