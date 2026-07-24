# jaspy-nexus WebSocket API

Real-time push stream for external applications: live discovery logs and
per-device change events, delivered as JSON frames over a standard WebSocket.
This is a read-only fan-out — the server pushes, the client listens.

Applies to jaspy-nexus v2.2.0. The HTTP/REST companion is documented in
[`nexus/API.md`](../nexus/API.md); use REST for state snapshots and this
WebSocket for live updates on top of them.

## Endpoint

```
GET /api/v1/ws/logs/{topic}      (WebSocket upgrade)
```

Served on the same host and port as the HTTP API. Pick the scheme to match how
you reach the HTTP API:

| HTTP API reached over | WebSocket scheme |
|---|---|
| `http://`  | `ws://`  |
| `https://` (TLS terminated by a proxy) | `wss://` |

Examples:

```
ws://jaspy.example.com/api/v1/ws/logs/discovery
wss://jaspy.example.com/api/v1/ws/logs/device%3Asw1.example.com
```

`{topic}` is a single URL path segment. Topics that contain a colon
(`device:<fqdn>`) work either raw or percent-encoded (`%3A`); the server
percent-decodes the segment before matching. Encoding is the safer choice
because some client libraries mangle raw `:` in a path.

### Handshake, framing, keepalive

- Standard [RFC 6455](https://www.rfc-editor.org/rfc/rfc6455) WebSocket. No
  subprotocol is negotiated and no custom headers are required.
- The server sends **text frames only**, each containing exactly one JSON
  object (UTF-8). One frame = one message; there is no framing of your own to
  do beyond `JSON.parse` per frame.
- On connect, the server first replays the topic's **backlog** (if any — see
  per-topic notes), then streams new frames as they are published.
- The server sends a WebSocket **Ping every 30 seconds**. Reply with a Pong
  (virtually all client libraries do this automatically). Pings let the server
  notice peers that vanished without closing; a client that never pongs may be
  dropped by intermediaries.
- **Client → server messages are ignored.** You never need to send anything.
  The server reads the socket only to detect that you have gone away.

### Authentication

There is currently **no authentication** on `/api/v1` (including this
WebSocket). Protect it at the network layer (VPN, reverse proxy, firewall).
`/api/v1` is the surface intended to sit behind authentication in a future
release; when that lands, this document will describe how to present
credentials on the WebSocket handshake.

## Topics

Any topic name is accepted — subscribing to an unknown topic simply yields a
silent stream (an empty backlog and no frames). Three topics are actively
published to:

### `discovery` — discovery run log

Human-readable log lines emitted while the topology discovery crawler runs.

- **Frame shape:** `{ "ts": <number>, "line": <string> }`
  - `ts` — Unix timestamp in **seconds** as a float (millisecond resolution,
    e.g. `1690000000.123`).
  - `line` — one log line, without a trailing newline.
- **Backlog:** yes. The last **500** lines are retained and replayed to every
  new subscriber, so recent history is visible immediately on connect.

```json
{"ts": 1690000000.123, "line": "[discovery] LINK core1.example.com:Te1/0/1 -> dist1.example.com:Te1/1/1"}
```

### `device:{fqdn}` — per-device change events

Change events for one monitored device, where `{fqdn}` is the device's fully
qualified name (e.g. `device:sw1.example.com`).

- **Frame shape:** an `Event` object (see [Event frames](#event-frames)).
- **Backlog:** **none.** These are live-only. Events are not even serialized
  unless at least one client is currently subscribed to that device's topic (or
  to `devices`, below) — so subscribe *before* you expect to observe an event,
  and treat anything that happened while you were disconnected as missed
  (re-sync via REST; see [Reconnecting](#reconnecting-and-missed-events)).

### `devices` — all device events (fleet-wide)

Every device change event, for **all** devices, on a single connection — the
one-socket feed for an external integration that wants to mirror fleet state.

- **Frame shape:** identical to `device:{fqdn}` — an `Event` object (see
  [Event frames](#event-frames)). Each event carries its own `fqdn`, so no
  extra envelope is needed to tell devices apart.
- **Backlog:** **none** (live-only), same as the per-device topics.
- **Scope:** carries exactly the same events as the per-device topics, merged.
  Subscribing to **both** `devices` and a specific `device:{fqdn}` delivers each
  matching event **twice** — pick one or the other.

```
ws://jaspy.example.com/api/v1/ws/logs/devices
```

## Event frames

A device event is a JSON object with two always-present fields plus exactly one
payload object whose key names the variant:

| Field | Type | Meaning |
|---|---|---|
| `eventType` | string | Discriminator; equals the payload key below. |
| `createTime` | number | Unix timestamp in **seconds** (float, ms resolution). |
| *(one payload key)* | object | The variant payload; see the table. Only the key matching `eventType` is present. |

### Event types

| `eventType` / payload key | Payload fields | Fires when |
|---|---|---|
| `pingChange` | `fqdn`, `neighbors: string[]`, `oldState: bool`, `newState: bool` | The device's reachability (ICMP) state flips. `neighbors` are the fqdns of directly-linked devices. |
| `interfaceUpDown` | `fqdn`, `name`, `neighbor: string?`, `neighborName: string?`, `neighborLinksState: {string: string}`, `oldState: bool`, `newState: bool` | An interface's operational state flips. `neighbor`/`neighborName` identify the far end when known. |
| `interfaceSpeed` | `fqdn`, `name`, `neighbor: string?`, `neighborName: string?`, `oldState: int`, `newState: int` | An interface's negotiated speed changes. |
| `devicePollingChanged` | `fqdn`, `oldState: bool?`, `newState: bool?` | Monitoring/polling was enabled or disabled for the device. |
| `deviceOsInfoChanged` | `fqdn`, `oldState: string?`, `newState: string?` | The device's reported OS/software string changed. |
| `deviceBaseMacChanged` | `fqdn`, `oldState: string?`, `newState: string?` | The chassis base MAC changed (hardware swap behind the same fqdn). |
| `deviceCreated` | `fqdn` | A device was added to inventory. |
| `deviceDeleted` | `fqdn` | A device was removed from inventory. |

Fields typed `?` are nullable/optional. `bool` states are `true` = up/enabled.
All payloads carry `fqdn`, matching the topic you subscribed to.

Example (`device:sw1.example.com`):

```json
{
  "eventType": "interfaceUpDown",
  "createTime": 1690000000.500,
  "interfaceUpDown": {
    "fqdn": "sw1.example.com",
    "name": "GigabitEthernet1/0/1",
    "neighbor": "sw2.example.com",
    "neighborName": "GigabitEthernet1/0/2",
    "neighborLinksState": {"GigabitEthernet1/0/1": "down"},
    "oldState": true,
    "newState": false
  }
}
```

> Forward-compatibility: treat `eventType` as an open set and **ignore unknown
> event types and unknown fields**. New event types and fields may be added
> without a version bump.

## Delivery semantics

- **At-most-once, best-effort.** There is no acknowledgement or replay of
  individual frames.
- **Slow consumers may lose frames.** Each topic keeps a **1024**-frame buffer.
  A subscriber that falls more than 1024 frames behind is fast-forwarded: the
  skipped frames are dropped silently (no error, no gap marker) and streaming
  continues with the newest frames. Keep your `onmessage` handler cheap, or
  buffer and process off the socket thread.
- **Ordering** is preserved within a topic (except across a skip as above).
- Frames are independent of MQTT. When an MQTT broker is configured, the same
  device events are *also* published to `jaspy/nexus/<eventType>`; the
  WebSocket stream works whether or not MQTT is enabled.

## Reconnecting and missed events

Networks and server restarts will drop the connection. Reconnect with a capped
exponential backoff (the bundled web UI uses 500 ms → 5 s, and retries
immediately on tab focus / network return).

Because the `device:{fqdn}` and `devices` topics have **no backlog**, any event
that occurred while you were disconnected is gone. After every (re)connect,
re-synchronise authoritative state from REST before trusting the live stream
again:

- Device state: `GET /api/v1/devices/{fqdn}` (live up/down, interfaces,
  health).
- Device list / inventory: `GET /api/v1/devices`.

For `discovery`, the backlog replays the last 500 lines on reconnect, so you
may **see lines you already processed** — de-duplicate on `(ts, line)` if that
matters to you.

## Client examples

### Browser / Node (JavaScript)

```js
const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
const topic = encodeURIComponent('device:sw1.example.com');
const ws = new WebSocket(`${proto}//${location.host}/api/v1/ws/logs/${topic}`);

ws.onmessage = (e) => {
  const event = JSON.parse(e.data);
  if (event.eventType === 'interfaceUpDown') {
    const p = event.interfaceUpDown;
    console.log(`${p.fqdn} ${p.name}: ${p.oldState} -> ${p.newState}`);
  }
};
ws.onclose = () => {/* reconnect with backoff, then re-fetch REST state */};
```

### Python (`websockets`)

```python
import asyncio, json, websockets

async def main():
    url = "ws://jaspy.example.com/api/v1/ws/logs/device%3Asw1.example.com"
    async with websockets.connect(url, ping_timeout=None) as ws:
        async for frame in ws:                     # library auto-pongs
            event = json.loads(frame)
            print(event["eventType"], event.get("createTime"))

asyncio.run(main())
```

### Shell (`websocat`)

```sh
# Tail discovery logs (backlog replays first, then live):
websocat ws://jaspy.example.com/api/v1/ws/logs/discovery

# One device's events:
websocat 'ws://jaspy.example.com/api/v1/ws/logs/device%3Asw1.example.com'

# Every device's events on one connection:
websocat ws://jaspy.example.com/api/v1/ws/logs/devices
```

## Source of truth

- Transport / route: `nexus/src/routes/api/v1.rs` (`ws_logs`).
- Topic registry, backlog, fan-out: `nexus/src/utilities/livelog.rs`.
- Event schema: `nexus/src/models/events.rs`.
