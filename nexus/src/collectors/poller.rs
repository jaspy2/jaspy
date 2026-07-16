// In-process SNMP interface-counter collector (formerly the `jaspy-poller`
// binary). Ported near-verbatim from poller/src/{poller,main,models/json}.rs;
// the only behavioral change is that per-device reports are written directly
// into IMDS (`report_interfaces`) rather than PUT to /dev/interface/monitor.
extern crate serde_json;

use crate::models;
use crate::db;
use crate::snmp::{HostSpec, SnmpSource};
use crate::utilities::imds::IMDS;
use crate::utilities::tools;
use rand::prelude::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, atomic, mpsc};
use std::thread;
use std::time;

// The snmpbot-shaped response models now live in crate::snmp::types; re-export
// them here so the many `crate::collectors::poller::SNMP*` references across
// the collectors, mock and tests keep working unchanged.
pub use crate::snmp::types::{
    SNMPBotObjectResponse, SNMPBotResponse, SNMPBotResultEntry, SNMPBotResultEntryObjectValue,
};

fn try_get_u64(val: Option<&SNMPBotResultEntryObjectValue>) -> Option<u64> {
    if let Some(SNMPBotResultEntryObjectValue::Uint64(val)) = val {
        return Some(*val);
    }
    return None;
}

fn try_get_i32(val: Option<&SNMPBotResultEntryObjectValue>) -> Option<i32> {
    if let Some(SNMPBotResultEntryObjectValue::Uint64(val)) = val {
        if *val > std::i32::MAX as u64 {
            // TODO: log? bad overflow :C
            return None;
        }
        return Some(*val as i32);
    }
    return None;
}

fn try_get_updown_as_bool(val: Option<&SNMPBotResultEntryObjectValue>) -> Option<bool> {
    if let Some(SNMPBotResultEntryObjectValue::Str(val)) = val {
        return Some(val.to_lowercase() == "up");
    }
    return None;
}

fn interface_report_from_entry(if_index: &i32, objects: &HashMap<String, SNMPBotResultEntryObjectValue>) -> models::json::InterfaceMonitorInterfaceReport {
    models::json::InterfaceMonitorInterfaceReport {
        if_index: *if_index,
        in_octets: try_get_u64(objects.get("IF-MIB::ifHCInOctets")),
        out_octets: try_get_u64(objects.get("IF-MIB::ifHCOutOctets")),
        in_unicast_packets: try_get_u64(objects.get("IF-MIB::ifHCInUcastPkts")),
        in_multicast_packets: try_get_u64(objects.get("IF-MIB::ifHCInMulticastPkts")),
        in_broadcast_packets: try_get_u64(objects.get("IF-MIB::ifHCInBroadcastPkts")),
        out_unicast_packets: try_get_u64(objects.get("IF-MIB::ifHCOutUcastPkts")),
        out_multicast_packets: try_get_u64(objects.get("IF-MIB::ifHCOutMulticastPkts")),
        out_broadcast_packets: try_get_u64(objects.get("IF-MIB::ifHCOutBroadcastPkts")),
        in_errors: try_get_u64(objects.get("IF-MIB::ifInErrors")),
        out_errors: try_get_u64(objects.get("IF-MIB::ifOutErrors")),
        out_discards: try_get_u64(objects.get("IF-MIB::ifOutDiscards")),
        up: try_get_updown_as_bool(objects.get("IF-MIB::ifOperStatus")),
        speed: try_get_i32(objects.get("IF-MIB::ifHighSpeed")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IFTABLE_JSON: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/iftable.json"));
    const IFXTABLE_JSON: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ifxtable.json"));

    fn merged_fixture_stats() -> HashMap<i32, HashMap<String, SNMPBotResultEntryObjectValue>> {
        let iftable: SNMPBotResponse = serde_json::from_str(IFTABLE_JSON).unwrap();
        let ifxtable: SNMPBotResponse = serde_json::from_str(IFXTABLE_JSON).unwrap();
        let mut stats = HashMap::new();
        merge_query_result(&mut stats, &iftable);
        merge_query_result(&mut stats, &ifxtable);
        stats
    }

    #[test]
    fn null_entries_decode_as_empty() {
        let response: SNMPBotResponse =
            serde_json::from_str(r#"{"ID": "IF-MIB::ifTable", "IndexKeys": null, "ObjectKeys": null, "Entries": null}"#).unwrap();
        assert!(response.entries.is_empty());
        assert!(response.index_keys.is_empty());
        assert!(response.object_keys.is_empty());
    }

    #[test]
    fn untagged_value_decoding() {
        let entry: SNMPBotResultEntry = serde_json::from_str(
            r#"{"HostID": "h", "Index": {}, "Objects": {
                "uint": 42,
                "float": 4.5,
                "str": "up",
                "bool": true,
                "empty": null,
                "other": {"Error": "timeout"}
            }}"#,
        ).unwrap();
        assert!(matches!(entry.objects.get("uint"), Some(SNMPBotResultEntryObjectValue::Uint64(42))));
        assert!(matches!(entry.objects.get("float"), Some(SNMPBotResultEntryObjectValue::Float64(_))));
        assert!(matches!(entry.objects.get("str"), Some(SNMPBotResultEntryObjectValue::Str(_))));
        assert!(matches!(entry.objects.get("bool"), Some(SNMPBotResultEntryObjectValue::Bool(true))));
        assert!(matches!(entry.objects.get("empty"), Some(SNMPBotResultEntryObjectValue::Empty)));
        assert!(matches!(entry.objects.get("other"), Some(SNMPBotResultEntryObjectValue::Other(_))));
    }

    #[test]
    fn try_get_u64_variants() {
        assert_eq!(try_get_u64(Some(&SNMPBotResultEntryObjectValue::Uint64(7))), Some(7));
        assert_eq!(try_get_u64(Some(&SNMPBotResultEntryObjectValue::Str("7".to_string()))), None);
        assert_eq!(try_get_u64(None), None);
    }

    #[test]
    fn try_get_i32_rejects_overflow() {
        assert_eq!(try_get_i32(Some(&SNMPBotResultEntryObjectValue::Uint64(1000))), Some(1000));
        assert_eq!(try_get_i32(Some(&SNMPBotResultEntryObjectValue::Uint64(i32::MAX as u64))), Some(i32::MAX));
        assert_eq!(try_get_i32(Some(&SNMPBotResultEntryObjectValue::Uint64(i32::MAX as u64 + 1))), None);
        assert_eq!(try_get_i32(None), None);
    }

    #[test]
    fn try_get_updown_is_case_insensitive() {
        assert_eq!(try_get_updown_as_bool(Some(&SNMPBotResultEntryObjectValue::Str("up".to_string()))), Some(true));
        assert_eq!(try_get_updown_as_bool(Some(&SNMPBotResultEntryObjectValue::Str("UP".to_string()))), Some(true));
        assert_eq!(try_get_updown_as_bool(Some(&SNMPBotResultEntryObjectValue::Str("down".to_string()))), Some(false));
        assert_eq!(try_get_updown_as_bool(Some(&SNMPBotResultEntryObjectValue::Str("lowerLayerDown".to_string()))), Some(false));
        assert_eq!(try_get_updown_as_bool(Some(&SNMPBotResultEntryObjectValue::Uint64(1))), None);
        assert_eq!(try_get_updown_as_bool(None), None);
    }

    #[test]
    fn merge_combines_tables_per_ifindex() {
        let stats = merged_fixture_stats();
        assert_eq!(stats.len(), 2);
        let port = stats.get(&10101).unwrap();
        // Objects from both tables land under the same ifindex.
        assert!(matches!(port.get("IF-MIB::ifOperStatus"), Some(SNMPBotResultEntryObjectValue::Str(_))));
        assert!(matches!(port.get("IF-MIB::ifHCInOctets"), Some(SNMPBotResultEntryObjectValue::Uint64(1000000))));
    }

    #[test]
    fn merge_first_writer_wins() {
        let first: SNMPBotResponse = serde_json::from_str(
            r#"{"ID": "t", "IndexKeys": [], "ObjectKeys": [], "Entries": [
                {"HostID": "h", "Index": {"IF-MIB::ifIndex": 1}, "Objects": {"IF-MIB::ifInErrors": 5}}
            ]}"#,
        ).unwrap();
        let second: SNMPBotResponse = serde_json::from_str(
            r#"{"ID": "t", "IndexKeys": [], "ObjectKeys": [], "Entries": [
                {"HostID": "h", "Index": {"IF-MIB::ifIndex": 1}, "Objects": {"IF-MIB::ifInErrors": 99}}
            ]}"#,
        ).unwrap();
        let mut stats = HashMap::new();
        merge_query_result(&mut stats, &first);
        merge_query_result(&mut stats, &second);
        assert!(matches!(stats.get(&1).unwrap().get("IF-MIB::ifInErrors"), Some(SNMPBotResultEntryObjectValue::Uint64(5))));
    }

    #[test]
    fn merge_skips_entries_without_ifindex() {
        let response: SNMPBotResponse = serde_json::from_str(
            r#"{"ID": "t", "IndexKeys": [], "ObjectKeys": [], "Entries": [
                {"HostID": "h", "Index": {}, "Objects": {"IF-MIB::ifInErrors": 5}}
            ]}"#,
        ).unwrap();
        let mut stats = HashMap::new();
        merge_query_result(&mut stats, &response);
        assert!(stats.is_empty());
    }

    #[test]
    fn interface_report_from_fixture() {
        let stats = merged_fixture_stats();
        let report = interface_report_from_entry(&10101, stats.get(&10101).unwrap());
        assert_eq!(report.if_index, 10101);
        assert_eq!(report.in_octets, Some(1000000));
        assert_eq!(report.out_octets, Some(2000000));
        assert_eq!(report.in_unicast_packets, Some(100));
        assert_eq!(report.out_broadcast_packets, Some(8));
        assert_eq!(report.in_errors, Some(0));
        assert_eq!(report.up, Some(true));
        assert_eq!(report.speed, Some(1000));

        let report = interface_report_from_entry(&10102, stats.get(&10102).unwrap());
        assert_eq!(report.up, Some(false));
        assert_eq!(report.in_errors, Some(2));
        assert_eq!(report.out_discards, Some(3));
    }

    #[test]
    fn interface_report_missing_objects_are_none() {
        let report = interface_report_from_entry(&1, &HashMap::new());
        assert_eq!(report.if_index, 1);
        assert_eq!(report.in_octets, None);
        assert_eq!(report.up, None);
        assert_eq!(report.speed, None);
    }
}

// --- collector internals ---

#[derive(Clone)]
struct PollDevice {
    fqdn: String,
    snmp_community: Option<String>,
}

struct PollThreadInfo {
    thd: thread::JoinHandle<()>,
    running: Arc<atomic::AtomicBool>,
    finished_signal: mpsc::Receiver<bool>,
}

fn load_devices(pool: &db::Pool) -> HashMap<String, PollDevice> {
    let mut devices: HashMap<String, PollDevice> = HashMap::new();
    if let Ok(mut conn) = pool.get() {
        for device in models::dbo::Device::monitored(&mut *conn).iter() {
            let fqdn = format!("{}.{}", device.name, device.dns_domain);
            devices.insert(fqdn.clone(), PollDevice { fqdn: fqdn, snmp_community: device.snmp_community.clone() });
        }
    } else {
        println!("[poller] failed to acquire db connection for device listing");
    }
    return devices;
}

fn snmp_query(device_fqdn: &str, statistics: &mut HashMap<i32, HashMap<String, SNMPBotResultEntryObjectValue>>, snmp: &SnmpSource, host: &HostSpec, table_id: &str) {
    match snmp.table(host, table_id) {
        Ok(query_result) => merge_query_result(statistics, &query_result),
        Err(what) => println!("[{}] snmp error for {} ({}), skipping this poll", device_fqdn, table_id, what),
    }
}

fn merge_query_result(statistics: &mut HashMap<i32, HashMap<String, SNMPBotResultEntryObjectValue>>, query_result: &SNMPBotResponse) {
    for query_result_entry in query_result.entries.iter() {
        let ifindex: i32;
        if let Some(ifindex_result) = query_result_entry.index.get("IF-MIB::ifIndex") {
            ifindex = *ifindex_result as i32;
        } else {
            continue;
        }

        let ifindex_stats = statistics.entry(ifindex).or_insert_with(HashMap::new);
        for (object_key, object_value) in query_result_entry.objects.iter() {
            // ifTable is queried before ifXTable; first writer wins on collision.
            if !ifindex_stats.contains_key(object_key) {
                ifindex_stats.insert(object_key.clone(), object_value.clone());
            }
        }
    }
}

fn poll_device(snmp: &SnmpSource, device: &PollDevice) -> Option<models::json::InterfaceMonitorReport> {
    let snmp_community = match device.snmp_community {
        Some(ref community) => community,
        // TODO: log?
        None => return None,
    };
    let host = HostSpec::with_community(&device.fqdn, snmp_community);

    let mut stats: HashMap<i32, HashMap<String, SNMPBotResultEntryObjectValue>> = HashMap::new();
    // ifTable before ifXTable: first writer wins on object-key collisions.
    snmp_query(&device.fqdn, &mut stats, snmp, &host, "IF-MIB::ifTable");
    snmp_query(&device.fqdn, &mut stats, snmp, &host, "IF-MIB::ifXTable");

    let mut report = models::json::InterfaceMonitorReport { device_fqdn: device.fqdn.clone(), interfaces: Vec::new() };
    for (ifindex, object_values) in stats.iter() {
        report.interfaces.push(interface_report_from_entry(ifindex, object_values));
    }
    return Some(report);
}

fn poll_worker(pool: db::Pool, snmp: Arc<SnmpSource>, device: PollDevice, poll_loop_msecs: u64, report_device_status: bool, imds: Arc<Mutex<IMDS>>, running: Arc<atomic::AtomicBool>, done: mpsc::Sender<bool>) {
    let no_jitter = std::env::var("JASPY_POLLER_NO_JITTER").map(|v| v == "1" || v == "true").unwrap_or(false);
    let start_sleep = if no_jitter { 0.0 } else { thread_rng().gen_range(0.0, poll_loop_msecs as f64) };
    println!("[{}] start polling thread, delay={:.2}ms", device.fqdn, start_sleep);
    thread::sleep(time::Duration::from_millis(start_sleep as u64));
    while running.load(atomic::Ordering::Relaxed) {
        let start = tools::get_time_msecs();

        println!("[{}] polling", device.fqdn);
        if let Some(poll_result) = poll_device(&snmp, &device) {
            // With the pinger disabled, a device that answers SNMP is up and
            // one that doesn't is down (empty result = snmpbot got no reply).
            let snmp_ok = !poll_result.interfaces.is_empty();
            // Do the DB acquire and IMDS lock only after network I/O, and hold
            // the IMDS lock only for the report itself.
            if let Ok(mut conn) = pool.get() {
                if let Ok(ref mut imds) = imds.lock() {
                    imds.report_interfaces(&mut *conn, poll_result);
                    if report_device_status {
                        imds.report_device(&mut *conn, models::json::DeviceMonitorReport {
                            fqdn: device.fqdn.clone(),
                            up: snmp_ok,
                        });
                    }
                }
            }
        }

        let diff = tools::get_time_msecs() - start;
        if diff <= poll_loop_msecs {
            thread::sleep(time::Duration::from_millis(poll_loop_msecs - diff));
        }
    }
    let _ = done.send(true);
    println!("[{}] stop polling", device.fqdn);
}

fn check_if_worker_needed(pool: &db::Pool, snmp: &Arc<SnmpSource>, poll_loop_msecs: u64, report_device_status: bool, imds: &Arc<Mutex<IMDS>>, devices: &HashMap<String, PollDevice>, poll_workers: &mut HashMap<String, PollThreadInfo>) {
    for (fqdn, device) in devices.iter() {
        if poll_workers.contains_key(fqdn) {
            continue;
        }
        let worker_running = Arc::new(atomic::AtomicBool::new(true));
        let running_worker = worker_running.clone();
        let (tx, rx) = mpsc::channel();
        let pool_copy = pool.clone();
        let snmp_copy = snmp.clone();
        let device_copy = device.clone();
        let imds_copy = imds.clone();
        poll_workers.insert(
            fqdn.clone(),
            PollThreadInfo {
                thd: thread::spawn(move || {
                    poll_worker(pool_copy, snmp_copy, device_copy, poll_loop_msecs, report_device_status, imds_copy, running_worker, tx);
                }),
                running: worker_running,
                finished_signal: rx,
            },
        );
    }
}

fn check_expired_fqdn_workers(devices: &HashMap<String, PollDevice>, poll_workers: &HashMap<String, PollThreadInfo>, expired_fqdns: &mut Vec<String>) {
    for (fqdn, poll_worker) in poll_workers.iter() {
        if devices.get(fqdn).is_none() {
            poll_worker.running.store(false, atomic::Ordering::Relaxed);
            expired_fqdns.push(fqdn.clone());
        }
    }
}

fn prepare_expired_fqdns_for_reap(poll_workers: &mut HashMap<String, PollThreadInfo>, expired_fqdns: &Vec<String>, reap_threads: &mut Vec<PollThreadInfo>) {
    for expired_fqdn in expired_fqdns.iter() {
        if let Some(poll_worker) = poll_workers.remove(expired_fqdn) {
            reap_threads.push(poll_worker);
        }
    }
}

fn reap_finished_threads(reap_threads: &mut Vec<PollThreadInfo>) {
    loop {
        let mut reap = false;
        let mut idx = 0;
        for reap_thread in reap_threads.iter() {
            match reap_thread.finished_signal.try_recv() {
                Ok(_) => { reap = true; break; },
                Err(mpsc::TryRecvError::Empty) => { idx += 1; },
                Err(mpsc::TryRecvError::Disconnected) => { reap = true; break; },
            }
        }
        if reap {
            let reaped = reap_threads.swap_remove(idx);
            let _ = reaped.thd.join();
        } else {
            break;
        }
    }
}

pub fn run(snmp: Arc<SnmpSource>, poll_loop_msecs: u64, report_device_status: bool, imds: Arc<Mutex<IMDS>>, running: Arc<atomic::AtomicBool>) {
    println!("[poller] starting in-process collector (poll_loop_msecs={})", poll_loop_msecs);
    if report_device_status {
        println!("[poller] pinger is disabled; deriving device up/down from SNMP poll replies");
    }
    let pool = db::connect();
    let mut poll_workers: HashMap<String, PollThreadInfo> = HashMap::new();
    let mut reap_threads: Vec<PollThreadInfo> = Vec::new();

    while running.load(atomic::Ordering::Relaxed) {
        let devices = load_devices(&pool);
        let mut expired_fqdns: Vec<String> = Vec::new();
        check_if_worker_needed(&pool, &snmp, poll_loop_msecs, report_device_status, &imds, &devices, &mut poll_workers);
        check_expired_fqdn_workers(&devices, &poll_workers, &mut expired_fqdns);
        prepare_expired_fqdns_for_reap(&mut poll_workers, &expired_fqdns, &mut reap_threads);
        reap_finished_threads(&mut reap_threads);
        thread::sleep(time::Duration::from_millis(1000));
    }

    // Graceful shutdown: signal and join every worker.
    for (_fqdn, worker) in poll_workers.iter() {
        worker.running.store(false, atomic::Ordering::Relaxed);
    }
    for (_fqdn, worker) in poll_workers.drain() {
        let _ = worker.thd.join();
    }
    for worker in reap_threads.drain(..) {
        let _ = worker.thd.join();
    }
    println!("[poller] collector stopped");
}
