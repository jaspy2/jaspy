// The SNMP access seam the collectors depend on. One of two back ends is
// constructed once at startup (main.rs) from `snmp_mode` and shared as
// Arc<SnmpSource>: the default snmpbot HTTP client, or the embedded snmp2
// client. Both return the same snmpbot-shaped response types, so the
// collectors are identical regardless of mode.
use super::embedded::Embedded;
use super::hostspec::HostSpec;
use super::snmpbot_http::SnmpbotHttp;
use super::types::{SNMPBotObjectResponse, SNMPBotResponse};

pub enum SnmpSource {
    SnmpbotHttp(SnmpbotHttp),
    Embedded(Embedded),
}

impl SnmpSource {
    pub fn table(&self, host: &HostSpec, table_id: &str) -> Result<SNMPBotResponse, String> {
        match self {
            SnmpSource::SnmpbotHttp(client) => client.table(host, table_id),
            SnmpSource::Embedded(client) => client.table(host, table_id),
        }
    }

    pub fn object(&self, host: &HostSpec, object_id: &str) -> Result<SNMPBotObjectResponse, String> {
        match self {
            SnmpSource::SnmpbotHttp(client) => client.object(host, object_id),
            SnmpSource::Embedded(client) => client.object(host, object_id),
        }
    }
}
