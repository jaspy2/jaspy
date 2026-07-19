// SNMP access layer. Two interchangeable back ends behind `SnmpSource`:
//   * snmpbot_http — the external snmpbot HTTP service (default; unchanged).
//   * embedded     — an in-process snmp2 (v2c) client + MIB registry that
//                    reproduces snmpbot's request set and value encodings.
// Selection is runtime config (main.rs), not a cargo feature, so one binary
// serves both deployment styles.
pub mod types;

pub mod hostspec;
pub mod raw;
pub mod mib;
pub mod render;
pub mod embedded;
pub mod snmpbot_http;
pub mod source;
pub mod trap;

pub use hostspec::HostSpec;
pub use source::{SnmpBackend, SnmpSource};

// Classify an SNMP backend error (both back ends return Result<_, String>) as a
// "device did not respond" timeout rather than a genuine error (bad MIB name,
// HTTP status, JSON parse). snmpbot surfaces "SNMP timeout: device not
// responding" (see snmpbot_http::describe_snmpbot_failure); the embedded back
// end surfaces the OS/UDP timeout text — both contain "timeout"/"timed out".
// Used to keep unresponsive-device timeouts out of the perf averages/overruns.
pub fn is_snmp_timeout(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("timeout") || e.contains("timed out")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_strings_classified() {
        // snmpbot's normalized reason and a raw OS/UDP timeout both count.
        assert!(is_snmp_timeout("SNMP timeout: device not responding"));
        assert!(is_snmp_timeout("recv: operation timed out"));
        // Genuine errors are not timeouts.
        assert!(!is_snmp_timeout("status=404: BRIDGE-MIB name not found: jaspyStpBridgeTable"));
        assert!(!is_snmp_timeout("json: expected value (body: ...)"));
        assert!(!is_snmp_timeout(""));
    }
}
