// The SNMP access seam the collectors depend on. One of two back ends is
// constructed once at startup (main.rs) from `snmp_mode` and shared as
// Arc<SnmpSource>: the default snmpbot HTTP client, or the embedded snmp2
// client. Both return the same snmpbot-shaped response types, so the
// collectors are identical regardless of mode.
//
// SnmpSource also owns an optional global in-flight limiter (PERF.md #4): every
// collector's SNMP call passes through here, so a single semaphore caps the
// total concurrent requests all collectors (interface poller + entity/vlan/lag)
// may have outstanding toward the shared back end. It also records concurrency
// (peak in-flight) and permit-wait time for the perf suite.
use super::embedded::Embedded;
use super::hostspec::HostSpec;
use super::snmpbot_http::SnmpbotHttp;
use super::types::{SNMPBotObjectResponse, SNMPBotResponse};
use crate::utilities::perfstats::PERF;
use crate::utilities::semaphore::Semaphore;
use std::time::Instant;

pub enum SnmpBackend {
    SnmpbotHttp(SnmpbotHttp),
    Embedded(Embedded),
}

pub struct SnmpSource {
    backend: SnmpBackend,
    // None = unlimited (preserves the historical behaviour); Some(n) caps total
    // concurrent SNMP requests to n across all collectors.
    limiter: Option<Semaphore>,
}

// Increments the in-flight gauge on entry and decrements on drop, so the peak
// reflects actual concurrent back-end calls even if one panics.
struct InflightGuard;

impl InflightGuard {
    fn enter() -> InflightGuard {
        PERF.inflight_enter();
        InflightGuard
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        PERF.inflight_exit();
    }
}

impl SnmpSource {
    // max_inflight = 0 means unlimited.
    pub fn new(backend: SnmpBackend, max_inflight: usize) -> SnmpSource {
        SnmpSource {
            backend,
            limiter: if max_inflight > 0 { Some(Semaphore::new(max_inflight)) } else { None },
        }
    }

    // Acquire an in-flight permit (if a cap is configured), timing the wait, and
    // mark the request as in the back end. The returned guards release the
    // permit and decrement the gauge on drop.
    fn admit(&self) -> (Option<crate::utilities::semaphore::SemaphoreGuard>, InflightGuard) {
        let permit = self.limiter.as_ref().map(|s| {
            let start = Instant::now();
            let guard = s.acquire();
            PERF.record_permit_wait(start.elapsed());
            guard
        });
        (permit, InflightGuard::enter())
    }

    pub fn table(&self, host: &HostSpec, table_id: &str) -> Result<SNMPBotResponse, String> {
        let _admit = self.admit();
        match &self.backend {
            SnmpBackend::SnmpbotHttp(client) => client.table(host, table_id),
            SnmpBackend::Embedded(client) => client.table(host, table_id),
        }
    }

    pub fn object(&self, host: &HostSpec, object_id: &str) -> Result<SNMPBotObjectResponse, String> {
        let _admit = self.admit();
        match &self.backend {
            SnmpBackend::SnmpbotHttp(client) => client.object(host, object_id),
            SnmpBackend::Embedded(client) => client.object(host, object_id),
        }
    }
}
