// Minimal snmpbot-compatible HTTP server backing `jaspy-nexus mock`.
//
// Deliberately a hand-rolled std::net::TcpListener server rather than rocket
// routes: the collector threads (and the immediately-due first discovery run)
// start before rocket binds its listener, so the fake snmpbot must accept
// connections from time zero. The surface is two GET patterns; thread-per-
// connection is plenty for one crawl's worth of parallelism.
//
// Request forms served (query string ignored — snmp=community@fqdn carries
// nothing the topology needs):
//   GET /api/hosts/{host}/tables/{table-id}
//   GET /api/hosts/{host}/objects/{object-id}
// where {host} is `fqdn` (poller, discovery), `community@fqdn` or
// `community@vlan@fqdn` (entitypoller).
use super::topology::Topology;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

#[derive(Debug, PartialEq)]
pub enum Kind {
    Table,
    Object,
}

#[derive(Debug, PartialEq)]
pub struct MockRequest {
    pub fqdn: String,
    pub vlan: Option<i64>,
    pub kind: Kind,
    pub id: String,
}

pub fn parse_path(path: &str) -> Option<MockRequest> {
    let path = path.split('?').next().unwrap_or("");
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    // ["api", "hosts", host, kind, id]
    if parts.len() != 5 || parts[0] != "api" || parts[1] != "hosts" {
        return None;
    }
    let host_parts: Vec<&str> = parts[2].split('@').collect();
    let (fqdn, vlan) = match host_parts.as_slice() {
        [fqdn] => (fqdn.to_string(), None),
        [_community, fqdn] => (fqdn.to_string(), None),
        [_community, vlan, fqdn] => (fqdn.to_string(), vlan.parse::<i64>().ok()),
        _ => return None,
    };
    let kind = match parts[3] {
        "tables" => Kind::Table,
        "objects" => Kind::Object,
        _ => return None,
    };
    Some(MockRequest { fqdn, vlan, kind, id: parts[4].to_string() })
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        status,
        body.len(),
        body
    );
}

fn handle(topology: &Topology, mut stream: TcpStream) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return,
    });
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    // Drain headers so the client sees a clean close.
    let mut header = String::new();
    while reader.read_line(&mut header).is_ok() {
        if header == "\r\n" || header == "\n" || header.is_empty() {
            break;
        }
        header.clear();
    }

    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or("");
    let path = request_parts.next().unwrap_or("");
    if method != "GET" {
        respond(&mut stream, "405 Method Not Allowed", "{}");
        return;
    }

    let request = match parse_path(path) {
        Some(r) => r,
        None => {
            respond(&mut stream, "404 Not Found", "{}");
            return;
        }
    };

    let elapsed = crate::utilities::tools::get_time() - topology.started;
    let body = match request.kind {
        Kind::Table => topology
            .table(&request.fqdn, request.vlan, &request.id, elapsed)
            .map(|resp| serde_json::to_string(&resp).unwrap()),
        Kind::Object => topology.object(&request.fqdn, &request.id).map(|value| {
            serde_json::json!({
                "ID": request.id,
                "Instances": [{"HostID": request.fqdn, "Value": value}],
            })
            .to_string()
        }),
    };
    match body {
        Some(body) => respond(&mut stream, "200 OK", &body),
        None => respond(&mut stream, "404 Not Found", "{}"),
    }
}

// Binds immediately (so the first discovery run can't race the listener) and
// serves forever on background threads.
pub fn spawn(topology: Arc<Topology>, port: u16) -> std::io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let bound_port = listener.local_addr()?.port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(stream) = stream {
                let topology = topology.clone();
                std::thread::spawn(move || handle(&topology, stream));
            }
        }
    });
    Ok(bound_port)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::topology;

    #[test]
    fn parse_plain_fqdn_table() {
        let req = parse_path("/api/hosts/core1.mock.jaspy/tables/IF-MIB::ifTable?snmp=mock@core1.mock.jaspy").unwrap();
        assert_eq!(req.fqdn, "core1.mock.jaspy");
        assert_eq!(req.vlan, None);
        assert_eq!(req.kind, Kind::Table);
        assert_eq!(req.id, "IF-MIB::ifTable");
    }

    #[test]
    fn parse_inline_community_host() {
        let req = parse_path("/api/hosts/mock@core1.mock.jaspy/tables/ENTITY-MIB::entPhysicalTable").unwrap();
        assert_eq!(req.fqdn, "core1.mock.jaspy");
        assert_eq!(req.vlan, None);
    }

    #[test]
    fn parse_per_vlan_host() {
        let req = parse_path("/api/hosts/mock@10@core1.mock.jaspy/tables/BRIDGE-MIB::dot1dBasePortTable").unwrap();
        assert_eq!(req.fqdn, "core1.mock.jaspy");
        assert_eq!(req.vlan, Some(10));
    }

    #[test]
    fn parse_objects_kind() {
        let req = parse_path("/api/hosts/core1.mock.jaspy/objects/SNMPv2-MIB::sysDescr?snmp=x").unwrap();
        assert_eq!(req.kind, Kind::Object);
        assert_eq!(req.id, "SNMPv2-MIB::sysDescr");
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_path("/api/hosts/core1.mock.jaspy/tables").is_none());
        assert!(parse_path("/api/nothosts/x/tables/y").is_none());
        assert!(parse_path("/api/hosts/x/frobnicate/y").is_none());
        assert!(parse_path("/").is_none());
    }

    // Full loopback: bind an ephemeral port, fetch with the same blocking
    // reqwest the collectors use, parse with the collector structs.
    #[test]
    fn serves_tables_and_objects_over_http() {
        let topo = std::sync::Arc::new(topology::build());
        let port = spawn(topo, 0).unwrap();
        let base = format!("http://127.0.0.1:{}", port);

        let body = reqwest::blocking::get(format!("{}/api/hosts/core1.mock.jaspy/tables/IF-MIB::ifXTable?snmp=mock@core1.mock.jaspy", base))
            .unwrap()
            .text()
            .unwrap();
        let parsed: crate::collectors::poller::SNMPBotResponse = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed.entries.len(), 3); // core1 has 3 uplinks

        let object = reqwest::blocking::get(format!("{}/api/hosts/core1.mock.jaspy/objects/SNMPv2-MIB::sysDescr", base))
            .unwrap()
            .json::<serde_json::Value>()
            .unwrap();
        assert_eq!(object["Instances"][0]["Value"].as_str().unwrap(), "Cisco IOS-XE Software (mock), Catalyst 9600 Switch");

        let missing = reqwest::blocking::get(format!("{}/api/hosts/ghost.mock.jaspy/tables/IF-MIB::ifTable", base)).unwrap();
        assert_eq!(missing.status().as_u16(), 404);

        // Per-vlan STP table through the full stack.
        let stp = reqwest::blocking::get(format!("{}/api/hosts/mock@10@core1.mock.jaspy/tables/BRIDGE-MIB::dot1dBasePortTable", base))
            .unwrap()
            .text()
            .unwrap();
        let parsed: crate::collectors::poller::SNMPBotResponse = serde_json::from_str(&stp).unwrap();
        assert!(!parsed.entries.is_empty());
    }
}
