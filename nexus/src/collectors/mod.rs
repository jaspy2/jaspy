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
