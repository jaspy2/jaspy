import { useEffect, useRef, useState } from 'react';
import { liveLogSocketUrl } from '../api/client';

// Reconnect backoff: start fast (a page load racing a server restart should
// recover in well under a second), cap low so "eventually" is never far away.
const BACKOFF_MIN_MS = 500;
const BACKOFF_MAX_MS = 5000;
// Abort a handshake stuck in CONNECTING (e.g. server mid-restart).
const CONNECT_TIMEOUT_MS = 5000;

export interface LiveSocketOptions {
  // Called with each JSON-parsed frame (malformed frames are dropped).
  onMessage: (data: unknown) => void;
  // Called on every (re)connect, before any frames arrive — e.g. to reset
  // state that the server replays from its backlog.
  onOpen?: () => void;
  // Bump this counter when the page just did something that needs the live
  // socket (e.g. triggered a run): a disconnected socket retries immediately
  // instead of waiting out its backoff.
  reconnectSignal?: number;
}

// Subscription to a server live-update topic (/api/v1/ws/logs/<topic>) with
// automatic reconnect: capped exponential backoff, handshake timeout, and
// immediate retry on tab focus / visibility / network return.
export default function useLiveSocket(
  topic: string,
  { onMessage, onOpen, reconnectSignal = 0 }: LiveSocketOptions,
): { connected: boolean } {
  const [connected, setConnected] = useState(false);

  const socketRef = useRef<WebSocket | null>(null);
  const retryTimer = useRef<number | undefined>(undefined);
  const connectTimer = useRef<number | undefined>(undefined);
  const backoffMs = useRef(BACKOFF_MIN_MS);
  const disposed = useRef(false);
  const connectRef = useRef<() => void>(() => {});

  // Keep the latest callbacks without re-running the connection effect.
  const onMessageRef = useRef(onMessage);
  onMessageRef.current = onMessage;
  const onOpenRef = useRef(onOpen);
  onOpenRef.current = onOpen;

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
        onOpenRef.current?.();
      };
      socket.onmessage = (event) => {
        try {
          onMessageRef.current(JSON.parse(event.data));
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

  return { connected };
}
