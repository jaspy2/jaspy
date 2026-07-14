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
    let ifm = models::json::InterfaceMonitorReport {
        device_fqdn: hostname.clone(),
        interfaces: vec![models::json::InterfaceMonitorInterfaceReport {
            if_index: ifindex as i32,
            in_octets: None,
            out_octets: None,
            in_unicast_packets: None,
            in_multicast_packets: None,
            in_broadcast_packets: None,
            out_unicast_packets: None,
            out_multicast_packets: None,
            out_broadcast_packets: None,
            in_errors: None,
            out_errors: None,
            out_discards: None,
            up: Some(up),
            speed: None,
        }],
    };
    send_interface_event(jaspy_url, ifm);
}

fn handle_parsed_trap(jaspy_url: &str, unix_time: f64, hostname: &String, trap: HashMap<String, String>) {
    let trap_type = match trap.get("SNMPv2-MIB::snmpTrapOID") {
        Some(value) => value,
        None => {
            println!("failed to find OID in trap");
            println!("{:?}", trap);
            return;
        }
    };
    let is_link_up = trap_type.starts_with("IF-MIB::linkUp");
    let is_link_down = trap_type.starts_with("IF-MIB::linkDown");
    if is_link_up || is_link_down {
        let ifindex_ifmib: Option<i64> = trap.get("IF-MIB::ifIndex").and_then(|v| v.parse().ok());
        let ifindex_rfc1213mib: Option<i64> = trap.get("RFC1213-MIB::ifIndex").and_then(|v| v.parse().ok());
        let ifindex = match ifindex_ifmib.or(ifindex_rfc1213mib) {
            Some(ifindex) => ifindex,
            None => {
                println!("failed to find/parse ifIndex from trap");
                return;
            }
        };
        send_link_event(jaspy_url, unix_time, hostname, ifindex, is_link_up);
    }
}

fn handle_trap(jaspy_url: &str, trap: String, unix_time: f64) {
    let mut lines = trap.split("\n");

    let hostname = match lines.next() {
        Some(line) => line.trim(),
        None => return,
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

    handle_parsed_trap(jaspy_url, unix_time, &hostname.to_string(), trap_info);
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
