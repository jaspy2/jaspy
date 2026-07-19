// snmpbot HTTP client: the default SNMP back end. This is the reqwest access
// code that used to live inline in each collector, centralized behind one
// client so the collectors go through `SnmpSource`. Behavior is preserved:
// the URL forms are reconstructed from the HostSpec style, and errors carry
// the snmpbot response body (which holds the real reason, e.g. "SNMP timeout
// for GetNextRequest<...>").
//
// The reqwest *blocking* client owns a private tokio runtime, so it must be
// built off any async runtime thread. `SnmpbotHttp` builds one lazily on first
// use and reuses it (PERF.md #5); every SNMP call runs on a collector OS
// thread, never inside rocket's async context, so first-use init is safe.
use super::hostspec::HostSpec;
use super::types::{SNMPBotObjectResponse, SNMPBotResponse};
use std::sync::OnceLock;
use std::time::Duration;

pub struct SnmpbotHttp {
    base_url: String,
    // One shared blocking client, built once and reused (PERF.md #5). Building a
    // fresh client per request spun up a private tokio runtime and opened a new
    // TCP connection each time; reusing it keeps HTTP/1.1 keep-alive connections
    // to snmpbot warm and drops the per-request runtime/handshake cost.
    client: OnceLock<reqwest::blocking::Client>,
}

impl SnmpbotHttp {
    pub fn new(base_url: String) -> SnmpbotHttp {
        SnmpbotHttp { base_url, client: OnceLock::new() }
    }

    // The reqwest blocking client owns a private tokio runtime and so must be
    // built off any async runtime thread. Every SNMP call runs on a collector OS
    // thread (never inside rocket's async context), so lazily initializing on
    // first use here is safe. 60s timeout matches the discovery engine's
    // historical client; a generous ceiling that never fires under normal poll
    // intervals.
    fn client(&self) -> &reqwest::blocking::Client {
        self.client.get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .unwrap_or_else(|_| reqwest::blocking::Client::new())
        })
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
        let response = self.client().get(url).send().map_err(|e| format!("{}", e))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            return Err(describe_snmpbot_failure(status, &body));
        }
        let body = response.text().map_err(|e| format!("read: {}", e))?;
        serde_json::from_str(&body).map_err(|e| format!("json: {} (body: {:.200})", e, body))
    }

    pub fn object(&self, host: &HostSpec, object_id: &str) -> Result<SNMPBotObjectResponse, String> {
        let url = self.url(host, "objects", object_id)?;
        let response = self.client().get(url).send().map_err(|e| format!("{}", e))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            return Err(describe_snmpbot_failure(status, &body));
        }
        let body = response.text().map_err(|e| format!("read: {}", e))?;
        serde_json::from_str(&body).map_err(|e| format!("json: {} (body: {:.200})", e, body))
    }
}

// snmpbot answers HTTP 500 with a body like
//   "SNMP<[::]:55334> timeout for GetNextRequest<1.3.6.1.2.1.17.6, ...>"
// when the polled device does not answer SNMP within snmpbot's timeout. Turn
// that into a concise "device not responding" reason rather than surfacing the
// raw 500 status line + OID dump; other failures keep the status (+ body).
fn describe_snmpbot_failure(status: reqwest::StatusCode, body: &str) -> String {
    let body = body.trim();
    if body.to_ascii_lowercase().contains("timeout") {
        return "SNMP timeout: device not responding".to_string();
    }
    if body.is_empty() {
        return format!("status={}", status);
    }
    format!("status={}: {:.200}", status, body)
}

// Liveness probe for the Maintenance page's snmpbot status line. Does a short
// GET to the snmpbot API root and treats ANY HTTP response — even a 4xx — as
// "up": we only care that snmpbot is reachable and answering, not what it says
// (the mock snmpbot, for one, answers 404 there). A transport error
// (connection refused, DNS failure, timeout) means down.
//
// Runs on a dedicated OS thread because reqwest::blocking spins its own tokio
// runtime and would panic if called from rocket's async worker (see the client
// note above). The reqwest timeout bounds how long the join can block.
pub fn probe(base_url: &str, timeout: Duration) -> bool {
    let url = format!("{}/api/", base_url.trim_end_matches('/'));
    std::thread::spawn(move || {
        match reqwest::blocking::Client::builder().timeout(timeout).build() {
            Ok(client) => client.get(&url).send().is_ok(),
            Err(_) => false,
        }
    })
    .join()
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

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

    // snmpbot's real "device not responding" body (from prod logs) must become a
    // concise reason, not the raw 500 status line + OID dump.
    #[test]
    fn timeout_body_reads_as_device_not_responding() {
        let body = "SNMP<[::]:55334> timeout for GetNextRequest<1.3.6.1.2.1.17.6, \
                    1.3.6.1.4.1.9.12.1, 1.3.6.1.4.1.9, 1.3.6.1.4.1.6027>";
        assert_eq!(
            describe_snmpbot_failure(reqwest::StatusCode::INTERNAL_SERVER_ERROR, body),
            "SNMP timeout: device not responding",
        );
    }

    // Non-timeout failures (e.g. an unknown MIB table) keep the status + body so
    // the real reason is still visible.
    #[test]
    fn non_timeout_failure_keeps_status_and_body() {
        let msg = describe_snmpbot_failure(
            reqwest::StatusCode::NOT_FOUND,
            "BRIDGE-MIB name not found: jaspyStpBridgeTable",
        );
        assert!(msg.starts_with("status=404"), "got: {}", msg);
        assert!(msg.contains("name not found"), "got: {}", msg);
    }

    #[test]
    fn empty_failure_body_shows_status_only() {
        assert_eq!(
            describe_snmpbot_failure(reqwest::StatusCode::BAD_GATEWAY, "   "),
            "status=502 Bad Gateway",
        );
    }

    // A responding snmpbot is "up" even when it answers 404 (the mock does):
    // the probe only checks that an HTTP response came back at all.
    #[test]
    fn probe_true_when_server_responds_even_404() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 512];
                let _ = stream.read(&mut buf); // drain the request line + headers
                let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\n\r\n{}");
            }
        });
        assert!(probe(&format!("http://127.0.0.1:{}", port), Duration::from_secs(2)));
        let _ = server.join();
    }

    // Nothing listening -> connection refused -> down.
    #[test]
    fn probe_false_when_unreachable() {
        // Bind then drop to obtain a port guaranteed to have no listener.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(!probe(&format!("http://127.0.0.1:{}", port), Duration::from_secs(2)));
    }
}
