// In-process collectors, formerly the standalone `jaspy-poller` and
// `jaspy-pinger` binaries. Each exposes a `run(...)` supervisor that is spawned
// as a background thread from `main()` and reconciles one worker thread per
// monitored device, reporting results directly into the shared IMDS instead of
// PUTing over localhost HTTP.
pub mod poller;
pub mod pinger;
// entitypoller is a stateless Prometheus exporter (entity sensors + STP) whose
// samples surface through the shared /dev/metrics endpoint rather than IMDS.
pub mod entitypoller;
// discovery is the topology crawler (formerly the Python `discover` tool); it
// runs on manual trigger (POST /dev/discovery/run) or a periodic interval.
pub mod discovery;
// vlanpoller collects per-interface VLAN membership (native + tagged) into an
// in-memory store served by /api/v1 only — no DB, no Prometheus.
pub mod vlanpoller;
// vendor holds the shared vendor-source selection (hint from discovery data,
// per-device winner cache) used by entitypoller and vlanpoller.
pub mod vendor;
// pool bounds the per-device fan-out of the entity and VLAN collectors.
pub mod pool;
// lagpoller collects port-channel membership + LACP health into an in-memory
// store served by /api/v1 only — no DB, no Prometheus.
pub mod lagpoller;
// entity_media derives per-interface physical media/form-factor (copper vs SFP
// cage vs populated transceiver) from ENTITY-MIB; shared by discovery (DB
// baseline) and entitypoller (live overlay).
pub mod entity_media;
// poe decodes Power-over-Ethernet state: per-port status/class/power
// (POWER-ETHERNET-MIB + CISCO-POWER-ETHERNET-EXT-MIB) and switch-wide PSE
// budget; the entitypoller polls it into an in-memory overlay served by /api/v1.
pub mod poe;
// qos decodes Cisco Class-Based QoS (policy-map) counters
// (CISCO-CLASS-BASED-QOS-MIB) and owns the negative-probe cache that keeps the
// entitypoller from re-walking devices with no service-policies. Core-router
// feature; the entitypoller polls it into an overlay served by /api/v1.
pub mod qos;

// The embedded SNMP client (snmp2) embeds two 64 KiB receive buffers by value —
// `SyncSession { recv_buf: [u8; 65507] }` and each request `Pdu { buf: [u8;
// 65507] }`. Opening a session and building request PDUs stacks several of these,
// and a debug build (which doesn't elide the redundant 64 KiB moves the way
// release does) overflows the default 2 MiB thread stack on the very first poll.
// Give every thread that opens SNMP sessions a generous stack. Thread stacks are
// committed lazily, so the unused headroom costs address space, not RSS.
pub const SNMP_WORKER_STACK_BYTES: usize = 8 * 1024 * 1024;

// A thread builder with the SNMP worker stack size. Use this (instead of
// `thread::spawn`) for any thread that polls, discovers, or otherwise opens
// embedded SNMP sessions.
pub fn snmp_thread_builder() -> std::thread::Builder {
    std::thread::Builder::new().stack_size(SNMP_WORKER_STACK_BYTES)
}
