# jaspy-nexus API

Machine-readable HTTP API for jaspy-nexus (v2.2.0).

## Two API surfaces

jaspy exposes two overlapping HTTP surfaces:

- **`/api/v1`** — the newer JSON API introduced with the embedded web admin UI
  (2026-07-14). It is the intended end-user / integration surface and the
  single prefix meant to go behind authentication later. Recommended for new
  integrations. Documented first, below.
- **`/dev/*`** — the original jaspy API, predating `/api/v1`. It is a
  **maintained, supported surface** and must be kept working: external tools
  depend on it, the in-network agents (device/interface monitors, discovery
  crawler, DHCP snooping) push data in through it, the PIXI weathermap reads
  from it, and Prometheus scrapes `/dev/metrics`. Some handlers carry
  "TODO: GH#9 move everything to v1" markers, but that migration must not break
  `/dev/*` for its external consumers — treat it as a stable contract, not a
  deprecated one. Documented in its own section, below.

> **Maintenance note:** changes to `/dev/*` endpoints or their request/response
> shapes are breaking changes for external integrations. Keep them
> backward-compatible; add fields additively, do not remove or rename existing
> ones.

The two surfaces share model shapes in places but are **not** identical: e.g.
`/dev/device` returns the raw DB [`Device`](#device)/[`Interface`](#interface)
rows, while `/api/v1/devices` returns the enriched [`ApiDevice`](#apidevice)
(live up/down, health, interface counts).

- **Base URL:** `http://<host>:8000` by default (Rocket; override with
  `ROCKET_ADDRESS` / `ROCKET_PORT`). Mock mode listens on `127.0.0.1:8000`.
- **Prefixes:** the first section is under `/api/v1`; the legacy surface is
  under `/dev`. Each endpoint's full path is given.
- **Auth:** none currently on either surface. `/api/v1` is the single boundary
  intended to go behind authentication later — do not assume it is
  unauthenticated forever.
- **Content type:** requests and responses are `application/json` (except the
  WebSocket and the Prometheus metrics endpoint). Send `Content-Type:
  application/json` on requests with a body.
- **Field naming:** all JSON keys are `camelCase`.
- **Compatibility:** response objects are additive — new fields may appear
  without a version bump. Clients must ignore unknown fields and tolerate
  missing optional (`null`) fields.
- **`null`:** a field typed `T | null` below is present as `null` when absent.

## Conventions

### Errors

Non-2xx responses that carry a body use:

```json
{ "error": "human-readable message" }
```

The web UI surfaces `error` verbatim. Some failures (e.g. a not-found `GET`)
return a bare status with no body — see each endpoint.

### Status codes

| Code | Meaning |
|------|---------|
| 200 | OK, body follows |
| 202 | Accepted — work was queued, not completed synchronously |
| 400 | Bad request (e.g. discovery not configured) |
| 404 | Resource not found (unknown fqdn / vlan), or a create/update/delete that could not complete |
| 409 | Conflict — the requested operation is already in progress or disabled |
| 500 | Internal error (body carries `error`) |

### Time & units

- `startupTime`, `lastStarted`, `lastFinished`: Unix time in **seconds**
  (fractional `f64`).
- `secondsSinceLastPoll`, all `*WindowSecs`, `*SecsAgo`,
  `timeSinceTopologyChangeSecs`, `discoveryIntervalSecs`: **seconds**.
- Sensor / STP `timestamp`, and `*Msecs` fields: **milliseconds**.
- Counters (`inOctets`, `inErrors`, …): cumulative since the device's last
  counter reset, as reported by SNMP (`ifHCInOctets` etc.).

---

## System & summary

### `GET /api/v1/summary`

Dashboard header: version, device roll-up, current event, discovery status.

Response: [`ApiSummary`](#apisummary).

### `GET /api/v1/system`

Effective feature configuration and live connection state (DB, MQTT, pollers,
discovery scheduling). Performs a live 500 ms DB probe.

Response: [`ApiSystemStatus`](#apisystemstatus).

### `GET /api/v1/system/perf`

Core hot-path performance counters (lifetime totals; diff successive polls for
live rates). Times are pre-divided to milliseconds.

Response: [`ApiPerfStats`](#apiperfstats).

### `GET /api/v1/system/env`

The `JASPY_*` environment the process is running with, for an admin config view.
Values are redacted server-side: SNMP communities and `*_PASSWORD`/`_SECRET`/
`_TOKEN`/`_APIKEY` are masked, and credentials embedded in URL values are
stripped. Sorted by name.

Response: array of [`ApiEnvVar`](#apienvvar).

---

## Devices

### `GET /api/v1/devices`

All devices with live up/down and a worst-interface health badge.

Response: array of [`ApiDevice`](#apidevice).

### `GET /api/v1/devices/{fqdn}`

Full device detail: device, interfaces (with live state, counters, VLANs,
port-channel membership, per-interface health), the device VLAN catalog,
port-channels, and the addresses the fqdn currently resolves to.

- `404` (no body) when the fqdn is unknown.

Response: [`ApiDeviceDetail`](#apidevicedetail).

### `GET /api/v1/devices/{fqdn}/entity`

Latest entitypoller results (environment sensors + per-VLAN STP) from the
in-memory store. Unknown fqdn and not-yet-polled devices both return `200`
with empty arrays.

Response: [`ApiDeviceEntity`](#apideviceentity).

### `POST /api/v1/devices/{fqdn}/vlans/poll`

Queue an immediate VLAN-membership poll for one device; fresh data lands in
`GET /devices/{fqdn}` within a couple of seconds.

- `202` — queued.
- `404` — unknown device, or device has no SNMP community.
- `409` — a poll for this device is already in progress, or the vlanpoller is
  disabled (`JASPY_ENABLE_VLANPOLLER`).

No request body. `202` has an empty body; errors return [`ApiError`](#apierror).

### `POST /api/v1/devices`

Create a device.

Request: [`NewDevice`](#newdevice). Response: [`ApiDevice`](#apidevice).
Returns `404` (no body) if creation fails.

### `PUT /api/v1/devices/{fqdn}`

Update a device. Only `pollingEnabled`, `osInfo`, `baseMac`, and
`snmpCommunity` are applied; other fields in the body are ignored. Changing
`baseMac` resets the device's in-memory interface counters (chassis-swap
handling).

Request: [`NewDevice`](#newdevice). Response: [`ApiDevice`](#apidevice).
`404` (no body) when the fqdn is unknown.

### `DELETE /api/v1/devices/{fqdn}`

Delete a device (cascades interfaces, client locations, weathermap position).

Response: the deleted [`Device`](#device) (raw DB row, **not** `ApiDevice`).
`404` (no body) when the fqdn is unknown or the delete fails.

---

## VLANs & STP

### `GET /api/v1/vlans`

Network-wide VLAN inventory aggregated across every polled device (in-memory
vlanpoller store). Empty until the first poll.

Response: array of [`ApiVlanSummary`](#apivlansummary).

### `GET /api/v1/stp`

Which VLANs have STP data, for a VLAN selector, with a resolved root per VLAN.

Response: array of [`ApiStpVlanSummary`](#apistpvlansummary).

### `GET /api/v1/stp/{vlan}`

The computed active spanning tree for one VLAN (`vlan` is an integer): nodes in
DFS order, blocked links, and structural anomaly flags.

Response: [`ApiStpTree`](#apistptree).

---

## Client locations

### `GET /api/v1/clientlocations`

All discovered client locations (DHCP option-82 records).

Response: array of [`ClientLocation`](#clientlocation).

---

## Event

The "event" is a free-text label for the current deployment (e.g. a venue).

### `GET /api/v1/event`

Response: [`ApiEvent`](#apievent).

### `PUT /api/v1/event`

Set (or clear, with `{"name": null}`) the event name.

Request / Response: [`ApiEvent`](#apievent). `500` with [`ApiError`](#apierror)
on persistence failure.

---

## Reset

### `POST /api/v1/reset`

Reset between events: delete **every** device (cascades interfaces, client
locations, weathermap positions) and clear the event name. Discovery config and
other settings are kept. No request body.

Response: [`ApiResetResult`](#apiresetresult).

---

## Discovery control

Mounted under `/api/v1/discovery`. Controls the in-process discovery engine.

### `POST /api/v1/discovery/run`

Trigger a discovery run. Optional body overrides the stored config for this run
only.

- `202` — accepted, run queued. Body: [`DiscoveryStatus`](#discoverystatus).
- `400` — discovery not configured (no root device / community from config or
  body). Body: [`ApiError`](#apierror).
- `409` — a run is already in progress. Body: [`ApiError`](#apierror).

Request (optional): [`DiscoveryRunRequest`](#discoveryrunrequest).

### `GET /api/v1/discovery/status`

Response: [`DiscoveryStatus`](#discoverystatus).

### `GET /api/v1/discovery/config`

Response: [`DiscoveryConfig`](#discoveryconfig).

### `PUT /api/v1/discovery/config`

Replace the discovery config (persisted; survives restarts and wins over the
`JASPY_DISCOVERY_*` env seed).

Request / Response: [`DiscoveryConfig`](#discoveryconfig). `500` with
[`ApiError`](#apierror) on persistence failure.

---

## Live updates (WebSocket)

### `GET /api/v1/ws/logs/{topic}`

WebSocket stream. On connect the server replays the topic's backlog, then
streams frames as they are published. Sends a keepalive ping every 30 s;
inbound client messages are ignored.

Topics:

- `discovery` — discovery run log lines: `{"ts": <number>, "line": "<string>"}`.
  Includes backlog.
- `device:{fqdn}` — msgbus events for one device (see `models/events.rs`);
  live-only, no backlog.
- `devices` — every device's events on one connection (same frames as
  `device:{fqdn}`, merged); live-only, no backlog.

Full protocol reference (frame shapes, event types, delivery semantics,
reconnect guidance, client examples): [`docs/WEBSOCKET.md`](../docs/WEBSOCKET.md).

---

# `/dev` API

The original jaspy HTTP surface — a maintained, supported API consumed by
external tools and by jaspy's own in-network agents (see the maintenance note
above). Read endpoints return raw DB rows ([`Device`](#device),
[`Interface`](#interface), [`ClientLocation`](#clientlocation)) or in-memory
DTOs. **Ingest** endpoints (the `PUT`s that agents push to) are best-effort:
they return `200` with an empty body and silently drop malformed or unmatched
input rather than erroring. Same JSON + camelCase conventions as `/api/v1`.

## Devices (`/dev/device`)

| Method & path | Body | Response | Notes |
|---|---|---|---|
| `GET /dev/device/` | — | [`Device`](#device)`[]` | all devices (raw rows) |
| `GET /dev/device/{fqdn}` | — | [`Device`](#device) | `404` if unknown |
| `POST /dev/device/` | [`NewDevice`](#newdevice) | [`Device`](#device) | `404` on failure. Emits a device-created event |
| `PUT /dev/device/{fqdn}` | [`NewDevice`](#newdevice) | [`Device`](#device) | only `pollingEnabled`/`osInfo`/`baseMac`/`snmpCommunity` applied; `404` if unknown |
| `DELETE /dev/device/{fqdn}` | — | [`Device`](#device) | deleted row; cascades; `404` if unknown |
| `GET /dev/device/{fqdn}/interfaces` | — | [`Interface`](#interface)`[]` | interfaces of one device (empty if unknown fqdn) |
| `GET /dev/device/monitor` | — | [`DeviceMonitorResponse`](#devicemonitorresponse) | monitored set + last-known up state + `stateId`; polled by the ping-monitor agent |
| `PUT /dev/device/monitor` | [`DeviceMonitorReport`](#devicemonitorreport) | empty | agent reports one device up/down (ingest) |
| `GET /dev/device/{fqdn}/status` | — | [`DeviceStatus`](#devicestatus) | live up state from IMDS; `404` if not in IMDS |
| `GET /dev/device/{fqdn}/status/interfaces` | — | [`DeviceInterfaceStatus`](#deviceinterfacestatus)`[]` | live per-interface state; `404` if not in IMDS |
| `DELETE /dev/device/connections?device_fqdn={fqdn}` | — | empty | clears every link connection on the device (both ends) |

## Interfaces (`/dev/interface`)

| Method & path | Body | Response | Notes |
|---|---|---|---|
| `GET /dev/interface/?device_fqdn={fqdn}` | — | [`Interface`](#interface)`[]` | `device_fqdn` optional: all interfaces if omitted; empty if fqdn unknown |
| `PUT /dev/interface/monitor` | [`InterfaceMonitorReport`](#interfacemonitorreport) | empty | **core telemetry ingest** — the poll agent reports interface counters/up/speed; this feeds IMDS, `/dev/metrics`, and interface health |

## Client locations (`/dev/clientlocation`)

| Method & path | Body | Response | Notes |
|---|---|---|---|
| `GET /dev/clientlocation/?client_address={ip}` | — | [`ClientLocation`](#clientlocation) | `404` if the IP is not known |
| `PUT /dev/clientlocation/` | [`ClientLocationInfo`](#clientlocationinfo) | empty | DHCP option-82 snooping ingest: maps a client IP to a switch port via option-82 sub-options `001`/`002` and the switch base MAC. Silently ignored if `001`/`002` are missing or the base MAC matches no device |

## Discovery (`/dev/discovery`)

`POST /run`, `GET /status`, `GET /config`, `PUT /config` are the **same
handlers** as [`/api/v1/discovery`](#discovery-control) (documented above),
also mounted here. Two ingest endpoints exist only under `/dev`:

| Method & path | Body | Response | Notes |
|---|---|---|---|
| `PUT /dev/discovery/device` | [`DiscoveredDevice`](#discovereddevice) | empty | crawler upserts a discovered device + its interfaces |
| `PUT /dev/discovery/links` | [`LinkInfo`](#linkinfo) | empty | crawler reports interface adjacencies (links) |

## Weathermap (`/dev/weathermap`)

Feeds the standalone PIXI weathermap front-end.

| Method & path | Body | Response | Notes |
|---|---|---|---|
| `GET /dev/weathermap/` | — | [`WeathermapBase`](#weathermapbase) | device + interface topology with link peers; TTL-cached |
| `GET /dev/weathermap/state` | — | [`WeathermapStateBase`](#weathermapstatebase) | per-device and per-neighbor-interface up/down (only `neighbors="yes"` interfaces), derived from fast metrics |
| `GET /dev/weathermap/position` | — | [`WeathermapPositionInfoBase`](#weathermappositioninfobase) | saved node positions |
| `PUT /dev/weathermap/position` | [`WeathermapPositionInfoUpdateDeviceInfo`](#weathermappositioninfoupdatedeviceinfo) | empty | upsert one device's position |

## Prometheus metrics (`/dev/metrics`)

Prometheus text exposition (not JSON). The machine-readable telemetry surface
for scraping.

- `GET /dev/metrics/fast` — device/interface up/down only (cheap, scrape often):
  `jaspy_device_up`, `jaspy_interface_up`.
- `GET /dev/metrics` — full counters: `jaspy_interface_octets`,
  `jaspy_interface_unicast_packets`, `jaspy_interface_multicast_packets`,
  `jaspy_interface_broadcast_packets`, `jaspy_interface_errors`,
  `jaspy_interface_discards`, `jaspy_interface_speed`, plus entitypoller
  sensor/STP metrics.
- `GET /dev/metrics/perf` — internal hot-path performance counters.

Interface series are labelled `fqdn`, `hostname`, `name`, `interfaceType`,
`neighbors`, and `direction` (`rx`/`tx`) for the directional counters.

---

## Schemas

Types: `integer`/`number` (JSON number), `string`, `boolean`, `T[]` (array),
`object` (map). `T | null` is nullable.

### ApiSummary

| Field | Type | Notes |
|-------|------|-------|
| `version` | string | server version |
| `stateId` | integer | monitored-set generation id |
| `startupTime` | number | Unix seconds |
| `eventName` | string \| null | current event label |
| `deviceCount` | integer | |
| `devicesUp` | integer | |
| `devicesDown` | integer | |
| `devicesUnknown` | integer | up/down not yet determined |
| `discovery` | [`DiscoveryStatus`](#discoverystatus) | |

### ApiDevice

| Field | Type | Notes |
|-------|------|-------|
| `id` | integer | |
| `fqdn` | string | `name.dnsDomain` |
| `name` | string | |
| `dnsDomain` | string | |
| `snmpCommunity` | string \| null | |
| `baseMac` | string \| null | chassis base MAC |
| `pollingEnabled` | boolean \| null | |
| `osInfo` | string \| null | |
| `deviceType` | string \| null | |
| `softwareVersion` | string \| null | |
| `up` | boolean \| null | live; null = indeterminate |
| `secondsSinceLastPoll` | integer \| null | null = never polled |
| `interfaceCount` | integer | |
| `interfaceHealth` | string \| null | worst severity `"warn"`/`"bad"`; null = all healthy |

### ApiDeviceDetail

| Field | Type | Notes |
|-------|------|-------|
| `device` | [`ApiDevice`](#apidevice) | |
| `interfaces` | [`ApiInterface`](#apiinterface)`[]` | sorted by `index` |
| `vlans` | [`ApiVlan`](#apivlan)`[]` | device VLAN catalog; empty until first VLAN poll |
| `portChannels` | [`ApiPortChannel`](#apiportchannel)`[]` | empty until first LAG poll |
| `ipAddresses` | string[] | resolved at request time, v4 first; empty when resolution fails |

### ApiInterface

| Field | Type | Notes |
|-------|------|-------|
| `id` | integer | |
| `index` | integer | ifIndex |
| `name` | string | |
| `displayName` | string \| null | |
| `alias` | string \| null | |
| `description` | string \| null | |
| `interfaceType` | string | SNMP ifType (e.g. `ethernetCsmacd`) |
| `pollingEnabled` | boolean \| null | |
| `speedOverride` | integer \| null | Mbps; wins over reported speed |
| `connectedTo` | [`ApiInterfaceConnection`](#apiinterfaceconnection) \| null | link peer |
| `up` | boolean \| null | live |
| `speed` | integer \| null | Mbps (effective: override or reported) |
| `inOctets` | integer \| null | cumulative |
| `outOctets` | integer \| null | cumulative |
| `inErrors` | integer \| null | cumulative |
| `outErrors` | integer \| null | cumulative |
| `outDiscards` | integer \| null | cumulative |
| `nativeVlan` | integer \| null | |
| `taggedVlans` | integer[] \| null | |
| `portChannel` | string \| null | name of the port-channel this is a member of |
| `health` | [`ApiInterfaceHealth`](#apiinterfacehealth) \| null | null when healthy |

### ApiInterfaceHealth

Present only when a signal has tripped.

| Field | Type | Notes |
|-------|------|-------|
| `severity` | string \| null | `"warn"` \| `"bad"`; null = healthy |
| `flapCount` | integer | in flap window |
| `lastFlapSecsAgo` | integer \| null | |
| `inErrors` | integer | over counter window |
| `outErrors` | integer | over counter window |
| `discards` | integer | over counter window |
| `speedChangeCount` | integer | |
| `lastSpeedChange` | [from, to] \| null | `[integer\|null, integer]` Mbps |
| `peakUtilizationPct` | number \| null | |
| `highUtilization` | boolean | |
| `rxBpsAvg` | number \| null | bits/sec over throughput window |
| `txBpsAvg` | number \| null | bits/sec over throughput window |
| `stale` | boolean | interface not polled recently |
| `counterWindowSecs` | integer | |
| `flapWindowSecs` | integer | |
| `utilWindowSecs` | integer | |
| `throughputWindowSecs` | integer | |

### ApiInterfaceConnection

| Field | Type | Notes |
|-------|------|-------|
| `fqdn` | string | peer device |
| `interface` | string | peer interface name |

### ApiVlan

| Field | Type | Notes |
|-------|------|-------|
| `id` | integer | VLAN id |
| `name` | string \| null | device's name for it |

### ApiPortChannel

| Field | Type | Notes |
|-------|------|-------|
| `ifindex` | integer | aggregate ifIndex |
| `name` | string \| null | aggregate interface name |
| `up` | boolean \| null | |
| `protocol` | string | `"lacp"` \| `"pagp"` \| `"static"` |
| `partnerSystemId` | string \| null | |
| `members` | [`ApiPortChannelMember`](#apiportchannelmember)`[]` | |
| `warnings` | string[] | mismatch codes (see `lagpoller`) |

### ApiPortChannelMember

| Field | Type | Notes |
|-------|------|-------|
| `ifindex` | integer | |
| `name` | string \| null | |
| `up` | boolean \| null | |
| `connectedTo` | [`ApiInterfaceConnection`](#apiinterfaceconnection) \| null | |
| `actorState` | string[] | IEEE 802.1AX LacpState bit names; empty when not running LACP |
| `partnerState` | string[] | |
| `partnerPort` | integer \| null | |
| `bundled` | boolean | sync + collecting + distributing all set |

### ApiDeviceEntity

| Field | Type | Notes |
|-------|------|-------|
| `sensors` | [`ApiEntitySensor`](#apientitysensor)`[]` | |
| `stp` | [`ApiStpPort`](#apistpport)`[]` | per-port STP |
| `stpBridges` | [`ApiStpBridge`](#apistpbridge)`[]` | per-VLAN bridge scalars |

### ApiEntitySensor

| Field | Type | Notes |
|-------|------|-------|
| `sensorId` | integer | |
| `name` | string | |
| `description` | string | |
| `value` | number | |
| `valueType` | string | ENTITY-SENSOR-MIB type (`celsius`, `voltsDC`, …) |
| `interfaceName` | string \| null | |
| `interfaceId` | integer \| null | |
| `timestamp` | integer | msecs |

### ApiStpPort

| Field | Type | Notes |
|-------|------|-------|
| `vlan` | integer | |
| `stpPortId` | integer | bridge port number |
| `interfaceName` | string \| null | |
| `interfaceId` | integer \| null | |
| `role` | string | `"designated"`, …, `"unknown"` |
| `state` | string | `"forwarding"`, …, `"unknown"` |
| `enabled` | boolean \| null | |
| `designatedCost` | integer | |
| `pathCost` | integer | |
| `priority` | integer | |
| `forwardTransitions` | integer | |
| `timestamp` | integer | msecs |

### ApiStpBridge

| Field | Type | Notes |
|-------|------|-------|
| `vlan` | integer | |
| `rootPriority` | integer \| null | |
| `rootMac` | string \| null | lowercase colon form |
| `rootCost` | integer \| null | |
| `rootPort` | integer \| null | bridge port number |
| `rootPortInterfaceName` | string \| null | |
| `topologyChanges` | integer \| null | |
| `timeSinceTopologyChangeSecs` | integer \| null | |
| `timestamp` | integer | msecs |

### ApiVlanSummary

| Field | Type | Notes |
|-------|------|-------|
| `id` | integer | |
| `names` | string[] | distinct names across devices; >1 = disagreement |
| `devices` | [`ApiVlanDevice`](#apivlandevice)`[]` | |

### ApiVlanDevice

| Field | Type | Notes |
|-------|------|-------|
| `fqdn` | string | |
| `name` | string \| null | device's name for the VLAN |
| `nativePorts` | integer | |
| `taggedPorts` | integer | |

### ApiStpVlanSummary

| Field | Type | Notes |
|-------|------|-------|
| `vlan` | integer | |
| `rootFqdn` | string \| null | resolved root device |
| `nodeCount` | integer | |
| `blockedPortCount` | integer | |
| `topologyChanges` | integer \| null | |
| `timeSinceTopologyChangeSecs` | integer \| null | |

### ApiStpTree

| Field | Type | Notes |
|-------|------|-------|
| `vlan` | integer | |
| `roots` | string[] | root fqdns |
| `nodes` | [`ApiStpNode`](#apistpnode)`[]` | DFS order |
| `blockedLinks` | [`ApiStpBlockedLink`](#apistpblockedlink)`[]` | |
| `flags` | string[] | `"no-root"`, `"multiple-roots"`, `"cycle"`, `"multiple-root-ports:<fqdn>"` |

### ApiStpNode

| Field | Type | Notes |
|-------|------|-------|
| `fqdn` | string | |
| `depth` | integer | |
| `parent` | string \| null | null for roots/orphans |
| `parentInterface` | string \| null | |
| `rootPortInterfaceName` | string \| null | |
| `rootPortState` | string \| null | |
| `pathCost` | integer \| null | |
| `reported` | [`ApiStpBridge`](#apistpbridge) \| null | |
| `rootMismatch` | boolean | reported root disagrees with computed root |
| `orphan` | boolean | root port present but upstream unresolved |

### ApiStpBlockedLink

| Field | Type | Notes |
|-------|------|-------|
| `fqdn` | string | |
| `stpPortId` | integer | unique per (fqdn, vlan) |
| `interfaceName` | string \| null | |
| `role` | string | `"alternate"` \| `"backUp"` |
| `state` | string | |
| `pathCost` | integer | |
| `connectedTo` | [`ApiInterfaceConnection`](#apiinterfaceconnection) \| null | |

### ApiSystemStatus

| Field | Type | Notes |
|-------|------|-------|
| `version` | string | |
| `startupTime` | number | Unix seconds |
| `snmpbotUrl` | string | |
| `snmpbotConnected` | boolean \| null | live reachability probe; null in embedded mode |
| `snmpMode` | string | `"snmpbot"` \| `"embedded"` |
| `snmpMibDir` | string \| null | |
| `snmpMibsLoaded` | integer \| null | |
| `trapReceiverEnabled` | boolean | |
| `trapBindAddress` | string \| null | |
| `dbUrl` | string | redacted |
| `dbBackend` | string | `"postgresql"` \| `"sqlite"` |
| `dbConnected` | boolean | live probe |
| `dbMigrationsPending` | boolean \| null | null when DB unreachable |
| `pollerEnabled` | boolean | |
| `pollLoopMsecs` | integer | |
| `pingerEnabled` | boolean | |
| `deviceStatusSource` | string | `"pinger"` \| `"poller"` |
| `entitypollerEnabled` | boolean | |
| `entitypollerIntervalMsecs` | integer | |
| `entitypollerSensorsEnabled` | boolean | |
| `entitypollerStpEnabled` | boolean | |
| `vlanpollerEnabled` | boolean | |
| `vlanpollerIntervalMsecs` | integer | |
| `lagpollerEnabled` | boolean | |
| `lagpollerIntervalMsecs` | integer | |
| `mqttEnabled` | boolean | |
| `mqttBroker` | string \| null | |
| `mqttConnected` | boolean \| null | |
| `discoveryPeriodicEnabled` | boolean | |
| `discoveryIntervalSecs` | integer | |
| `weathermapDir` | string \| null | |

### ApiPerfStats

Lifetime totals (diff successive polls for rates); `*Ms` fields pre-divided to
milliseconds.

| Field | Type |
|-------|------|
| `devicePolls` | integer |
| `pollOverruns` | integer |
| `pollIterMeanMs` | number |
| `pollIterMaxMs` | number |
| `snmpQueries` | integer |
| `snmpErrors` | integer |
| `snmpErrorPct` | number |
| `snmpMeanMs` | number |
| `snmpMaxMs` | number |
| `snmpSessionOpens` | integer |
| `snmpInflight` | integer |
| `snmpInflightMax` | integer |
| `imdsLockWaitMeanMs` | number |
| `imdsLockWaitMaxMs` | number |
| `imdsReportMeanMs` | number |
| `interfacesReported` | integer |
| `metricsScrapes` | integer |
| `metricsBuildMaxMs` | number |

### ApiEnvVar

One `JASPY_*` environment variable (value redacted; see `GET /system/env`).

| Field | Type |
|-------|------|
| `name` | string |
| `value` | string |

### DiscoveryStatus

| Field | Type | Notes |
|-------|------|-------|
| `running` | boolean | |
| `lastStarted` | number \| null | Unix seconds |
| `lastFinished` | number \| null | Unix seconds |
| `devicesFound` | integer \| null | |
| `devicesFailed` | integer \| null | |
| `linksFound` | integer \| null | |
| `lastError` | string \| null | |

### DiscoveryConfig

| Field | Type | Notes |
|-------|------|-------|
| `rootDevice` | string \| null | seed device fqdn |
| `community` | string \| null | SNMP community |
| `dnsDomains` | string[] | domains to crawl |
| `ignore` | string[] | fqdns to skip |
| `remap` | object (string→string) | fqdn rewrites |
| `topologyStable` | boolean | |
| `periodicEnabled` | boolean | |
| `intervalSecs` | integer | |

### DiscoveryRunRequest

All fields optional; override [`DiscoveryConfig`](#discoveryconfig) for one run.

| Field | Type |
|-------|------|
| `rootDevice` | string \| null |
| `community` | string \| null |
| `dnsDomains` | string[] \| null |
| `topologyStable` | boolean \| null |

### Device

Raw DB row (returned by `DELETE /devices/{fqdn}`).

| Field | Type |
|-------|------|
| `id` | integer |
| `name` | string |
| `dnsDomain` | string |
| `snmpCommunity` | string \| null |
| `baseMac` | string \| null |
| `pollingEnabled` | boolean \| null |
| `osInfo` | string \| null |
| `deviceType` | string \| null |
| `softwareVersion` | string \| null |

### NewDevice

Request body for `POST` / `PUT /devices`. (On `PUT`, only `pollingEnabled`,
`osInfo`, `baseMac`, `snmpCommunity` are applied.)

| Field | Type |
|-------|------|
| `name` | string |
| `dnsDomain` | string |
| `snmpCommunity` | string \| null |
| `baseMac` | string \| null |
| `pollingEnabled` | boolean \| null |
| `osInfo` | string \| null |
| `deviceType` | string \| null |
| `softwareVersion` | string \| null |

### ClientLocation

| Field | Type |
|-------|------|
| `id` | integer |
| `deviceId` | integer |
| `ipAddress` | string |
| `portInfo` | string |
| `hwAddress` | string |

### ApiEvent

| Field | Type |
|-------|------|
| `name` | string \| null |

### ApiResetResult

| Field | Type |
|-------|------|
| `devicesDeleted` | integer |

### ApiError

| Field | Type |
|-------|------|
| `error` | string |

### `/dev` schemas

#### Interface

Raw DB row (returned by `/dev/device/*/interfaces` and `/dev/interface`).

| Field | Type | Notes |
|-------|------|-------|
| `id` | integer | |
| `index` | integer | ifIndex |
| `interfaceType` | string | |
| `connectedInterface` | integer \| null | peer interface id (link) |
| `deviceId` | integer | owning device id |
| `displayName` | string \| null | |
| `name` | string | |
| `alias` | string \| null | |
| `description` | string \| null | |
| `pollingEnabled` | boolean \| null | |
| `speedOverride` | integer \| null | Mbps |
| `virtualConnection` | integer \| null | virtual peer interface id |

#### DeviceMonitorResponse

| Field | Type | Notes |
|-------|------|-------|
| `stateId` | integer | monitored-set generation id |
| `devices` | [`DeviceMonitorInfo`](#devicemonitorinfo)`[]` | |

#### DeviceMonitorInfo

| Field | Type | Notes |
|-------|------|-------|
| `fqdn` | string | |
| `up` | boolean \| null | last-known state |

#### DeviceMonitorReport

Ingest body for `PUT /dev/device/monitor`.

| Field | Type |
|-------|------|
| `fqdn` | string |
| `up` | boolean |

#### DeviceStatus

| Field | Type |
|-------|------|
| `fqdn` | string |
| `up` | boolean \| null |

#### DeviceInterfaceStatus

| Field | Type | Notes |
|-------|------|-------|
| `name` | string | |
| `neighbors` | boolean | has a discovered link peer |
| `up` | boolean \| null | |
| `speed` | integer \| null | Mbps (effective) |
| `interfaceType` | string | |

#### InterfaceMonitorReport

Ingest body for `PUT /dev/interface/monitor`.

| Field | Type | Notes |
|-------|------|-------|
| `deviceFqdn` | string | |
| `interfaces` | [`InterfaceMonitorInterfaceReport`](#interfacemonitorinterfacereport)`[]` | |

#### InterfaceMonitorInterfaceReport

| Field | Type | Notes |
|-------|------|-------|
| `ifIndex` | integer | |
| `inOctets` | integer \| null | cumulative; omit/null = unchanged |
| `outOctets` | integer \| null | |
| `inUnicastPackets` | integer \| null | |
| `inMulticastPackets` | integer \| null | |
| `inBroadcastPackets` | integer \| null | |
| `outUnicastPackets` | integer \| null | |
| `outMulticastPackets` | integer \| null | |
| `outBroadcastPackets` | integer \| null | |
| `inErrors` | integer \| null | |
| `outErrors` | integer \| null | |
| `outDiscards` | integer \| null | |
| `up` | boolean \| null | |
| `speed` | integer \| null | Mbps |

#### ClientLocationInfo

Ingest body for `PUT /dev/clientlocation/` (DHCP option-82).

| Field | Type | Notes |
|-------|------|-------|
| `yiaddr` | string | client IP |
| `chaddr` | string | client MAC |
| `option82` | object (string→string) | must contain `"001"` (6-octet colon hex: module/port) and `"002"` (8-octet colon hex: switch base MAC) |

#### DiscoveredDevice

Ingest body for `PUT /dev/discovery/device`.

| Field | Type | Notes |
|-------|------|-------|
| `name` | string | |
| `dnsDomain` | string | |
| `snmpCommunity` | string \| null | |
| `baseMac` | string \| null | |
| `osInfo` | string \| null | |
| `interfaces` | object (string→[`DiscoveredInterface`](#discoveredinterface)) | keyed by interface name |
| `deviceType` | string \| null | |
| `softwareVersion` | string \| null | |

#### DiscoveredInterface

| Field | Type |
|-------|------|
| `index` | integer |
| `interfaceType` | string |
| `displayName` | string \| null |
| `name` | string |
| `alias` | string \| null |
| `description` | string \| null |

#### LinkInfo

Ingest body for `PUT /dev/discovery/links`.

| Field | Type | Notes |
|-------|------|-------|
| `deviceFqdn` | string | |
| `interfaces` | object (string→([`LinkPeerInfo`](#linkpeerinfo) \| null)) | keyed by local interface name; null = no peer |
| `topologyStable` | boolean | |

#### LinkPeerInfo

| Field | Type |
|-------|------|
| `name` | string |
| `dnsDomain` | string |
| `interface` | string |

#### WeathermapBase

| Field | Type |
|-------|------|
| `devices` | object (string→[`WeathermapDevice`](#weathermapdevice)) |

#### WeathermapDevice

| Field | Type |
|-------|------|
| `fqdn` | string |
| `interfaces` | object (string→[`WeathermapDeviceInterface`](#weathermapdeviceinterface)) |

#### WeathermapDeviceInterface

| Field | Type |
|-------|------|
| `name` | string |
| `ifIndex` | integer |
| `connectedTo` | [`WeathermapDeviceInterfaceConnectedTo`](#weathermapdeviceinterfaceconnectedto) \| null |

#### WeathermapDeviceInterfaceConnectedTo

| Field | Type |
|-------|------|
| `fqdn` | string |
| `interface` | string |

#### WeathermapStateBase

| Field | Type |
|-------|------|
| `devices` | object (string→[`WeathermapStateDevice`](#weathermapstatedevice)) |

#### WeathermapStateDevice

| Field | Type | Notes |
|-------|------|-------|
| `state` | boolean | device up |
| `interfaces` | object (string→[`WeathermapStateDeviceInterfaceState`](#weathermapstatedeviceinterfacestate)) | neighbor interfaces only |

#### WeathermapStateDeviceInterfaceState

| Field | Type | Notes |
|-------|------|-------|
| `state` | boolean | interface up |

#### WeathermapPositionInfoBase

| Field | Type |
|-------|------|
| `devices` | object (string→[`WeathermapPositionInfoDeviceInfo`](#weathermappositioninfodeviceinfo)) |

#### WeathermapPositionInfoDeviceInfo

| Field | Type |
|-------|------|
| `x` | number |
| `y` | number |
| `superNode` | boolean |
| `expandedByDefault` | boolean |

#### WeathermapPositionInfoUpdateDeviceInfo

Ingest body for `PUT /dev/weathermap/position`.

| Field | Type |
|-------|------|
| `deviceFqdn` | string |
| `x` | number |
| `y` | number |
| `superNode` | boolean |
| `expandedByDefault` | boolean |
