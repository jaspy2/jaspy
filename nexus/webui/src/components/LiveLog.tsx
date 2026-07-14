import { useEffect, useRef, useState } from 'react';
import useLiveSocket from '../hooks/useLiveSocket';
import type { LiveLogLine } from '../api/types';

// Keep the view bounded; the server backlog is capped too (500 lines).
const MAX_LINES = 1000;

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
  const scrollRef = useRef<HTMLDivElement>(null);
  const followTail = useRef(true);

  const { connected } = useLiveSocket(topic, {
    // The server replays its backlog on every connect; start clean so a
    // reconnect does not duplicate history.
    onOpen: () => setLines([]),
    onMessage: (data) => {
      const entry = data as LiveLogLine;
      if (typeof entry?.line !== 'string') return;
      setLines((prev) => [...prev, entry].slice(-MAX_LINES));
    },
    reconnectSignal,
  });

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
