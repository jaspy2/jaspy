// snmpbot HTTP client: the default SNMP back end. This is the reqwest access
// code that used to live inline in each collector, centralized behind one
// client so the collectors go through `SnmpSource`. Behavior is preserved:
// the URL forms are reconstructed from the HostSpec style, and errors carry
// the snmpbot response body (which holds the real reason, e.g. "SNMP timeout
// for GetNextRequest<...>").
//
// The reqwest *blocking* client owns a private tokio runtime, so it must be
// built (and dropped) off any async runtime thread. `SnmpbotHttp` therefore
// holds only the base URL and constructs a client per request — exactly as the
// collectors did before (reqwest::blocking::get); every call runs on a
// collector OS thread, never inside rocket's async context.
use super::hostspec::HostSpec;
use super::types::{SNMPBotObjectResponse, SNMPBotResponse};
use std::time::Duration;

pub struct SnmpbotHttp {
    base_url: String,
}

impl SnmpbotHttp {
    pub fn new(base_url: String) -> SnmpbotHttp {
        SnmpbotHttp { base_url }
    }

    // 60s timeout matches the discovery engine's historical client; the other
    // collectors previously used the default (no timeout) blocking client, so
    // a generous ceiling is a strict safety improvement that never fires under
    // normal poll intervals.
    fn client() -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new())
    }

    fn url(&self, host: &HostSpec, kind: &str, id: &str) -> Result<reqwest::Url, String> {
        let source = format!("{}/api/hosts/{}/{}/{}", self.base_url, host.path_host(), kind, id);
        let mut parsed = reqwest::Url::parse(&source).map_err(|e| format!("bad url: {}", e))?;
        if let Some(snmp) = host.snmp_query() {
            parsed.query_pairs_mut().append_pair("snmp", &snmp);
        }
        Ok(parsed)
    }

    pub fn table(&self, host: &HostSpec, table_id: &str) -> Result<SNMPBotResponse, String> {
        let url = self.url(host, "tables", table_id)?;
        let response = Self::client().get(url).send().map_err(|e| format!("{}", e))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            let body = body.trim();
            if body.is_empty() {
                return Err(format!("status={}", status));
            }
            return Err(format!("status={}: {:.200}", status, body));
        }
        let body = response.text().map_err(|e| format!("read: {}", e))?;
        serde_json::from_str(&body).map_err(|e| format!("json: {} (body: {:.200})", e, body))
    }

    pub fn object(&self, host: &HostSpec, object_id: &str) -> Result<SNMPBotObjectResponse, String> {
        let url = self.url(host, "objects", object_id)?;
        let response = Self::client().get(url).send().map_err(|e| format!("{}", e))?;
        if !response.status().is_success() {
            return Err(format!("status={}", response.status()));
        }
        let body = response.text().map_err(|e| format!("read: {}", e))?;
        serde_json::from_str(&body).map_err(|e| format!("json: {} (body: {:.200})", e, body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_param_url_has_path_and_snmp() {
        let http = SnmpbotHttp::new("http://127.0.0.1:8286".to_string());
        let host = HostSpec::with_community("sw1.example.com", "public");
        let url = http.url(&host, "tables", "IF-MIB::ifTable").unwrap();
        assert_eq!(url.path(), "/api/hosts/sw1.example.com/tables/IF-MIB::ifTable");
        let pairs: Vec<(String, String)> = url.query_pairs().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        assert_eq!(pairs, vec![("snmp".to_string(), "public@sw1.example.com".to_string())]);
    }

    #[test]
    fn inline_url_has_no_query() {
        let http = SnmpbotHttp::new("http://127.0.0.1:8286".to_string());
        let host = HostSpec::parse("public@100@sw1.example.com");
        let url = http.url(&host, "tables", "BRIDGE-MIB::dot1dBasePortTable").unwrap();
        assert_eq!(url.path(), "/api/hosts/public@100@sw1.example.com/tables/BRIDGE-MIB::dot1dBasePortTable");
        assert_eq!(url.query(), None);
    }

    #[test]
    fn unparseable_base_errors() {
        let http = SnmpbotHttp::new("not a url".to_string());
        let host = HostSpec::with_community("sw1", "public");
        assert!(http.url(&host, "tables", "t").is_err());
    }
}
