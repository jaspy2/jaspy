#[macro_use] extern crate serde;
extern crate serde_json;
extern crate reqwest;
extern crate config;
extern crate nix;
use nix::unistd::{fork, ForkResult};
use std::io::{self, Read};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use config::{Config, File, Environment};

mod models;

fn send_interface_event(jaspy_url: String, ifm: models::json::InterfaceMonitorReport) {
    let client = reqwest::Client::new();
    let response = client.request(reqwest::Method::PUT, &format!("{}/dev/interface/monitor", jaspy_url))
        .json(&ifm)
        .send();
    match response {
        Ok(_) => {},
        Err(_) => {}
    }
}

fn send_link_up_event(jaspy_url: String, unix_time : f64, hostname : &String, ifindex : i64) {
    println!("linkup event @ {}: {} {}", unix_time, hostname, ifindex);
    let mut ifm = models::json::InterfaceMonitorReport {
        device_fqdn: hostname.clone(),
        interfaces: Vec::new()
    };
    ifm.interfaces.push(models::json::InterfaceMonitorInterfaceReport {
        if_index: ifindex as i32,
        up: true,
    });
    send_interface_event(jaspy_url, ifm);
}

fn send_link_down_event(jaspy_url: String, unix_time : f64, hostname : &String, ifindex : i64) {
    println!("linkdown event @ {}: {} {}", unix_time, hostname, ifindex);
    let mut ifm = models::json::InterfaceMonitorReport {
        device_fqdn: hostname.clone(),
        interfaces: Vec::new()
    };
    ifm.interfaces.push(models::json::InterfaceMonitorInterfaceReport {
        if_index: ifindex as i32,
        up: false,
    });
    send_interface_event(jaspy_url, ifm);
}

fn handle_parsed_trap(jaspy_url: String, unix_time : f64, hostname : &String, trap : HashMap<String, String>) {
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
        let ifindex_ifmib : Option<i64> = match trap.get("IF-MIB::ifIndex") {
            Some(value) => match value.parse() {
                Ok(value) => Some(value),
                Err(_) => None
            },
            None => None
        };
        let ifindex_rfc1213mib : Option<i64> = match trap.get("RFC1213-MIB::ifIndex") {
            Some(value) => match value.parse() {
                Ok(value) => Some(value),
                Err(_) => None
            },
            None => None
        };
        let ifindex;
        if let Some(ifindex_ifmib) = ifindex_ifmib {
            ifindex = ifindex_ifmib;
        } else if let Some(ifindex_rfc1213mib) = ifindex_rfc1213mib {
            ifindex = ifindex_rfc1213mib;
        } else {
            println!("failed to find/parse ifIndex from trap");
            return;
        }
        if is_link_up {
            send_link_up_event(jaspy_url, unix_time, hostname, ifindex);
        } else if is_link_down {
            send_link_down_event(jaspy_url, unix_time, hostname, ifindex);
        }
    }
}

fn handle_trap(jaspy_url: String, trap : String, unix_time : f64) {
    let mut lines = trap.split("\n");

    let hostname = match lines.next() {
        Some(line) => line.trim(),
        None => return
    };

    let mut trap_info : HashMap<String, String> = HashMap::new();

    loop {
        let line = match lines.next() {
            Some(line) => line.trim(),
            None => break
        };
        let line_splitted : Vec<&str> = line.splitn(2, " ").collect();
        if line_splitted.len() != 2 {
            continue;
        }
        let key : String = line_splitted[0].to_string();
        let key_mib : Vec<&str> = key.splitn(2, ".").collect();
        let _ifidx : i32;
        if key_mib.len() != 2 {
            _ifidx = 0;
        } else {
            _ifidx = match key_mib[1].parse() {
                Ok(value) => value,
                Err(_) => 0
            };
        }
        let mib : String = key_mib[0].to_string();
        let value : String = line_splitted[1].to_string();

        if !trap_info.contains_key(&mib) {
            trap_info.insert(mib, value);
        }
    }

    handle_parsed_trap(jaspy_url, unix_time, &hostname.to_string(), trap_info);
}

fn do_fork() {

}

fn get_unixtime_float_with_msecs() -> f64 {
    let start = SystemTime::now();
    match start.duration_since(UNIX_EPOCH) {
        Ok(unix_time) => {
            let seconds = unix_time.as_secs() as f64;
            let subseconds = unix_time.subsec_nanos() as f64 / 1_000_000_000.0;
            return seconds + subseconds;
        },
        Err(_) => { return 0.0; }
    }
}

fn main() {
    let mut c = Config::new();
    let jaspy_url;

    c.merge(File::with_name("/etc/jaspy/poller.yml").required(false)).unwrap()
        .merge(File::with_name("~/.config/jaspy/poller.yml").required(false)).unwrap()
        .merge(Environment::with_prefix("JASPY")).unwrap();

    match c.get_str("url") {
        Ok(v) => { jaspy_url = v },
        Err(_) => {
            if let Some(argv1) = std::env::args().nth(1) {
                jaspy_url = argv1;
            } else {
                println!("JASPY_URL not defined!");
                return;
            }
        },
    }

    let mut buffer_tmp = String::new();
    match io::stdin().read_to_string(&mut buffer_tmp) {
        Ok(_) => {
            let unix_time = get_unixtime_float_with_msecs();
            let buffer = buffer_tmp.clone();
            match fork() {
                Ok(ForkResult::Parent { child, .. }) => {
                },
                Ok(ForkResult::Child) => {
                    handle_trap(jaspy_url, buffer, unix_time);
                },
                Err(_) => {
                    println!("error: fork failed");
                }
            }
        },
        Err(e) => {
            println!("error: {:?}", e);
        }
    }
}
