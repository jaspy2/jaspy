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
