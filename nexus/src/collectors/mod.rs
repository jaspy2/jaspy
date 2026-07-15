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
