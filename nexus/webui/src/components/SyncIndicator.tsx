import type { LiveStatus } from '../hooks/useLiveSocket';

// Tooltip/aria text for each connection state. The dot itself is aria-hidden;
// this is what a screen reader announces and what the tooltip shows on hover.
const LABEL: Record<LiveStatus, string> = {
  live: 'Live — up to date',
  connecting: 'Reconnecting…',
  down: 'Offline — data may be stale',
};

// A small colored circle in the topbar reflecting the live socket: green =
// up to date, amber (pulsing) = reconnecting, red = offline. Clicking it forces
// an immediate resync so a user who suspects staleness can prod it.
export default function SyncIndicator({
  status,
  onResync,
}: {
  status: LiveStatus;
  onResync: () => void;
}) {
  const label = LABEL[status];
  return (
    <button
      type="button"
      className={`sync sync--${status}`}
      title={label}
      aria-label={label}
      onClick={onResync}
    >
      <span className="sync-dot" aria-hidden="true" />
    </button>
  );
}
