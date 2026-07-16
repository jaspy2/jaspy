// `jaspy-nexus trap-handler`: snmptrapd traphandle subcommand, formerly the
// standalone `jaspy-snmptrapd-reader` binary. snmptrapd fork-execs this per
// received linkUp/linkDown trap and pipes the trap text to stdin (line 1:
// hostname, line 2: transport address, then MIB-translated "KEY VALUE"
// lines). We fork so the parent returns to snmptrapd immediately while the
// child PUTs the up/down report to the running nexus over localhost HTTP —
// the same /dev/interface/monitor ingest the poller used before it moved
// in-process.
//
// This must run OUTSIDE any tokio runtime (reqwest::blocking + fork), which
// is why main() dispatches to it before rocket::execute.
use crate::models;
use crate::utilities::tools;
use config::{Config, Environment, File};
use std::collections::HashMap;
use std::io::Read;

fn send_interface_event(jaspy_url: &str, ifm: models::json::InterfaceMonitorReport) {
    let client = reqwest::blocking::Client::new();
    let response = client
        .put(&format!("{}/dev/interface/monitor", jaspy_url))
        .json(&ifm)
        .send();
    match response {
        Ok(_) => {},
        Err(_) => {}
    }
}

fn send_link_event(jaspy_url: &str, unix_time: f64, hostname: &String, ifindex: i64, up: bool) {
    println!("{} event @ {}: {} {}", if up { "linkup" } else { "linkdown" }, unix_time, hostname, ifindex);
    let ifm = models::json::InterfaceMonitorReport::link_event(hostname, ifindex as i32, up);
    send_interface_event(jaspy_url, ifm);
}

// Link state change carried by a trap: (ifIndex, is_up). None if the trap is
// not a linkUp/linkDown trap or lacks a usable ifIndex.
fn link_event_from_trap(trap: &HashMap<String, String>) -> Option<(i64, bool)> {
    let trap_type = match trap.get("SNMPv2-MIB::snmpTrapOID") {
        Some(value) => value,
        None => {
            println!("failed to find OID in trap");
            println!("{:?}", trap);
            return None;
        }
    };
    let is_link_up = trap_type.starts_with("IF-MIB::linkUp");
    let is_link_down = trap_type.starts_with("IF-MIB::linkDown");
    if is_link_up || is_link_down {
        let ifindex_ifmib: Option<i64> = trap.get("IF-MIB::ifIndex").and_then(|v| v.parse().ok());
        let ifindex_rfc1213mib: Option<i64> = trap.get("RFC1213-MIB::ifIndex").and_then(|v| v.parse().ok());
        match ifindex_ifmib.or(ifindex_rfc1213mib) {
            Some(ifindex) => Some((ifindex, is_link_up)),
            None => {
                println!("failed to find/parse ifIndex from trap");
                None
            }
        }
    } else {
        None
    }
}

fn handle_parsed_trap(jaspy_url: &str, unix_time: f64, hostname: &String, trap: HashMap<String, String>) {
    if let Some((ifindex, up)) = link_event_from_trap(&trap) {
        send_link_event(jaspy_url, unix_time, hostname, ifindex, up);
    }
}

fn parse_trap_text(trap: &str) -> Option<(String, HashMap<String, String>)> {
    let mut lines = trap.split("\n");

    let hostname = match lines.next() {
        Some(line) => line.trim(),
        None => return None,
    };

    let mut trap_info: HashMap<String, String> = HashMap::new();

    for line in lines {
        let line = line.trim();
        let line_splitted: Vec<&str> = line.splitn(2, " ").collect();
        if line_splitted.len() != 2 {
            continue;
        }
        // Keys arrive MIB-translated with an instance suffix
        // ("IF-MIB::ifIndex.0"); keep the part before the first '.'.
        let key: String = line_splitted[0].to_string();
        let key_mib: Vec<&str> = key.splitn(2, ".").collect();
        let mib: String = key_mib[0].to_string();
        let value: String = line_splitted[1].to_string();

        if !trap_info.contains_key(&mib) {
            trap_info.insert(mib, value);
        }
    }

    Some((hostname.to_string(), trap_info))
}

fn handle_trap(jaspy_url: &str, trap: String, unix_time: f64) {
    if let Some((hostname, trap_info)) = parse_trap_text(&trap) {
        handle_parsed_trap(jaspy_url, unix_time, &hostname, trap_info);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINKDOWN_TRAP: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/trap_linkdown.txt"));
    const LINKUP_TRAP: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/trap_linkup.txt"));

    #[test]
    fn parse_linkdown_fixture() {
        let (hostname, trap) = parse_trap_text(LINKDOWN_TRAP).unwrap();
        assert_eq!(hostname, "sw1.test.example");
        // Instance suffix (".0") is stripped from keys.
        assert_eq!(trap.get("SNMPv2-MIB::snmpTrapOID").map(String::as_str), Some("IF-MIB::linkDown.0"));
        assert_eq!(trap.get("IF-MIB::ifIndex").map(String::as_str), Some("10101"));
    }

    #[test]
    fn parse_first_key_wins() {
        let trap_text = "host.example\nIF-MIB::ifIndex.0 111\nIF-MIB::ifIndex.1 222\n";
        let (_, trap) = parse_trap_text(trap_text).unwrap();
        assert_eq!(trap.get("IF-MIB::ifIndex").map(String::as_str), Some("111"));
    }

    #[test]
    fn parse_skips_valueless_lines() {
        let trap_text = "host.example\njustakey\n\nIF-MIB::ifIndex.0 7\n";
        let (_, trap) = parse_trap_text(trap_text).unwrap();
        assert!(!trap.contains_key("justakey"));
        assert_eq!(trap.len(), 1);
    }

    #[test]
    fn link_event_linkdown() {
        let (_, trap) = parse_trap_text(LINKDOWN_TRAP).unwrap();
        assert_eq!(link_event_from_trap(&trap), Some((10101, false)));
    }

    #[test]
    fn link_event_linkup() {
        let (_, trap) = parse_trap_text(LINKUP_TRAP).unwrap();
        assert_eq!(link_event_from_trap(&trap), Some((10101, true)));
    }

    #[test]
    fn link_event_rfc1213_ifindex_fallback() {
        let mut trap = HashMap::new();
        trap.insert("SNMPv2-MIB::snmpTrapOID".to_string(), "IF-MIB::linkUp.0".to_string());
        trap.insert("RFC1213-MIB::ifIndex".to_string(), "42".to_string());
        assert_eq!(link_event_from_trap(&trap), Some((42, true)));
    }

    #[test]
    fn link_event_prefers_ifmib_ifindex() {
        let mut trap = HashMap::new();
        trap.insert("SNMPv2-MIB::snmpTrapOID".to_string(), "IF-MIB::linkDown.0".to_string());
        trap.insert("IF-MIB::ifIndex".to_string(), "1".to_string());
        trap.insert("RFC1213-MIB::ifIndex".to_string(), "2".to_string());
        assert_eq!(link_event_from_trap(&trap), Some((1, false)));
    }

    #[test]
    fn link_event_missing_oid_is_none() {
        let mut trap = HashMap::new();
        trap.insert("IF-MIB::ifIndex".to_string(), "42".to_string());
        assert_eq!(link_event_from_trap(&trap), None);
    }

    #[test]
    fn link_event_missing_ifindex_is_none() {
        let mut trap = HashMap::new();
        trap.insert("SNMPv2-MIB::snmpTrapOID".to_string(), "IF-MIB::linkDown.0".to_string());
        assert_eq!(link_event_from_trap(&trap), None);
    }

    #[test]
    fn link_event_unparseable_ifindex_is_none() {
        let mut trap = HashMap::new();
        trap.insert("SNMPv2-MIB::snmpTrapOID".to_string(), "IF-MIB::linkUp.0".to_string());
        trap.insert("IF-MIB::ifIndex".to_string(), "not-a-number".to_string());
        assert_eq!(link_event_from_trap(&trap), None);
    }

    #[test]
    fn non_link_trap_is_none() {
        let mut trap = HashMap::new();
        trap.insert("SNMPv2-MIB::snmpTrapOID".to_string(), "SNMPv2-MIB::coldStart.0".to_string());
        trap.insert("IF-MIB::ifIndex".to_string(), "42".to_string());
        assert_eq!(link_event_from_trap(&trap), None);
    }
}

pub fn run() {
    let c = Config::builder()
        .add_source(File::with_name("/etc/jaspy/poller.yml").required(false))
        .add_source(File::with_name("~/.config/jaspy/poller.yml").required(false))
        .add_source(Environment::with_prefix("JASPY"))
        .build()
        .unwrap();

    let jaspy_url = match c.get_string("url") {
        Ok(url) => url,
        Err(_) => {
            // args: [binary, "trap-handler", <url?>]
            match std::env::args().nth(2) {
                Some(url) => url,
                None => {
                    println!("JASPY_URL not defined!");
                    return;
                }
            }
        }
    };

    let mut buffer = String::new();
    match std::io::stdin().read_to_string(&mut buffer) {
        Ok(_) => {
            let unix_time = tools::get_time();
            // Fork so snmptrapd gets its handler back immediately; the child
            // performs the (blocking) HTTP report and exits. Safe: no runtime
            // or threads have been started on this code path.
            match unsafe { libc::fork() } {
                0 => {
                    // child
                    handle_trap(&jaspy_url, buffer, unix_time);
                },
                pid if pid > 0 => {
                    // parent: return to snmptrapd without waiting
                },
                _ => {
                    println!("error: fork failed");
                }
            }
        },
        Err(e) => {
            println!("error: {:?}", e);
        }
    }
}
