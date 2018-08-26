extern crate rand as crate_rand;
extern crate reqwest;
extern crate serde_json;
use crate_rand::prelude::*;
use models;
use std::sync;
use std::thread;
use std::time;
use util;
use HashMap;
use std::sync::{Arc, mpsc, atomic};

const POLL_LOOP_MSECS : u64 = 30000;

pub struct PollThreadInfo {
    pub thd : thread::JoinHandle<()>,
    pub running : Arc<atomic::AtomicBool>,
    pub finished_signal : mpsc::Receiver<bool>,
}

fn start_poll_worker(jaspy_url: &String, snmpbot_url: &String, device_info : &models::json::Device, ping_workers : &mut HashMap<String, PollThreadInfo>) {
    let running = Arc::new(sync::atomic::AtomicBool::new(true));
    let running_ptit = running.clone();
    let (tx, rx) = mpsc::channel();
    let device_info_copy = device_info.clone();
    let jaspy_url_copy = jaspy_url.clone();
    let snmpbot_url_copy = snmpbot_url.clone();
    let fqdn = format!("{}.{}", device_info.name, device_info.dns_domain);
    ping_workers.insert(
        fqdn.clone(), 
        PollThreadInfo {
            thd: thread::spawn(|| {
                poller(jaspy_url_copy, snmpbot_url_copy, device_info_copy, running_ptit, tx)
            }),
            running: running,
            finished_signal: rx
        }
    );
}

fn snmpbot_query(device_fqdn: &String, statistics: &mut HashMap<i32, HashMap<String, models::json::SNMPBotResultEntryObjectValue>>, url: &reqwest::Url) {
    let mut response;
    if let Ok(response_parsed) = reqwest::get(url.as_str()) {
        response = response_parsed;
    } else {
        // TODO: log?
        return;
    }
    if !response.status().is_success() {
        println!("[{}] snmpbot returned ({}), skipping this poll", device_fqdn, response.status());
        return;
    }
    let resp_json : Result<Vec<models::json::SNMPBotResponse>, _> = response.json();
    let mut query_results : Vec<models::json::SNMPBotResponse>;
    match resp_json {
        Ok(resp_json_result) => {
            query_results = resp_json_result;
        }, 
        Err(what) => {
            println!("[{}] error parsing json: {}", device_fqdn, what);
            return;
        }
    }
    let query_result: models::json::SNMPBotResponse;
    if let Some(first_query_result) = query_results.pop() {
        query_result = first_query_result;
    } else {
        // TODO: log?
        return;
    }

    for query_result_entry in query_result.entries.iter() {
        let ifindex : i32;
        if let Some(ifindex_result) = query_result_entry.index.get("IF-MIB::ifIndex") {
            ifindex = *ifindex_result as i32;
        } else {
            // TODO: log?
            continue;
        }

        let ifindex_stats : &mut HashMap<String, models::json::SNMPBotResultEntryObjectValue>;
        if statistics.contains_key(&ifindex) {
            if let Some(statistics_item) = statistics.get_mut(&ifindex) {
                ifindex_stats = statistics_item;
            } else {
                // TODO: log?
                continue;
            }
        } else {
            statistics.insert(ifindex, HashMap::new());
            if let Some(statistics_item) = statistics.get_mut(&ifindex) {
                ifindex_stats = statistics_item;
            } else {
                // TODO: log?
                continue;
            }
        }

        for (object_key, object_value) in query_result_entry.objects.iter() {
            if !ifindex_stats.contains_key(object_key) {
                ifindex_stats.insert(object_key.clone(), object_value.clone());
            }
        }
    }
}

fn poll_device(snmpbot_url: &String, device_info: &models::json::Device) -> Option<models::json::InterfaceMonitorReport> {
    let device_fqdn = format!("{}.{}", device_info.name, device_info.dns_domain);
    let source_url = format!("{}/api/hosts/{}/tables/", snmpbot_url, device_fqdn);
    let mut url;
    let snmp_community;
    if let Some(ref parsed_snmp_community) = device_info.snmp_community {
        snmp_community = parsed_snmp_community;
    } else {
        // TODO: log?
        return None;
    }
    if let Ok(parsed_url) = reqwest::Url::parse(&source_url) {
        url = parsed_url;
    } else {
        // TODO: log?
        return None;
    }
    url.query_pairs_mut().append_pair("snmp", &format!("{}@{}", snmp_community, device_fqdn));

    let mut iftable_url = url.clone();
    iftable_url.query_pairs_mut().append_pair("table", "IF-MIB::ifTable");

    let mut ifxtable_url = url.clone();
    ifxtable_url.query_pairs_mut().append_pair("table", "IF-MIB::ifXTable");

    let mut stats: HashMap<i32, HashMap<String, models::json::SNMPBotResultEntryObjectValue>> = HashMap::new();
    snmpbot_query(&device_fqdn, &mut stats, &iftable_url);
    snmpbot_query(&device_fqdn, &mut stats, &ifxtable_url);

    let mut report = models::json::InterfaceMonitorReport::new(&device_fqdn);

    for (ifindex, object_values) in stats.iter() {
        report.interfaces.push(models::json::InterfaceMonitorInterfaceReport::from_snmpbot_result_entry(ifindex, object_values));
    }

    return Some(report);
}

pub fn poller(jaspy_url : String, snmpbot_url: String, device_info : models::json::Device, running : sync::Arc<sync::atomic::AtomicBool>, done : sync::mpsc::Sender<bool>) {
    let fqdn = format!("{}.{}", device_info.name, device_info.dns_domain);
    let start_sleep = crate_rand::prelude::thread_rng().gen_range(0.0, POLL_LOOP_MSECS as f64);
    println!("[{}] start polling thread, delay={:.2}ms", fqdn, start_sleep);
    thread::sleep(time::Duration::from_millis(start_sleep as u64));
    while running.load(sync::atomic::Ordering::Relaxed) {
        let start = util::get_time_msecs();

        println!("[{}] polling", fqdn);
        if let Some(poll_result) = poll_device(&snmpbot_url, &device_info) {
            let client = reqwest::Client::new();
            let response = client.request(reqwest::Method::Put, &format!("{}/interface/monitor", jaspy_url))
                .json(&poll_result)
                .send();
            match response {
                Ok(_) => {},
                Err(_) => {}
            }
        }

        let diff = util::get_time_msecs() - start;
        let loop_time = POLL_LOOP_MSECS;
        if diff <= loop_time {
            thread::sleep(time::Duration::from_millis(loop_time - diff));
        }
    }
    done.send(true).unwrap();
    println!("[{}] stop polling", fqdn);
}

pub fn check_if_worker_needed(jaspy_url : &String, snmpbot_url: &String, devices : &HashMap<String, models::json::Device>, mut ping_workers : &mut HashMap<String, PollThreadInfo>) {
    for (fqdn, device_info) in devices.iter() {
        if ping_workers.contains_key(&*fqdn) {
            continue;
        }
        start_poll_worker(jaspy_url, snmpbot_url, &device_info, &mut ping_workers);
    }
}

pub fn reap_finished_threads(reap_threads : &mut Vec<PollThreadInfo>) {
    loop {
        let mut reap : bool = false;
        let mut idx = 0;
        for reap_thread in reap_threads.iter() {
            match reap_thread.finished_signal.try_recv() {
                Ok(_) => {
                    reap = true;
                    break;
                },
                Err(etype) => {
                    match etype {
                        mpsc::TryRecvError::Empty => {
                            idx += 1;
                        },
                        mpsc::TryRecvError::Disconnected => {
                            reap = true;
                            break;
                        }
                    }
                }
            }
        }
        if reap {
            let reaped_pti : PollThreadInfo = reap_threads.swap_remove(idx);
            reaped_pti.thd.join().unwrap();
        } else {
            break;
        }
    }
}

pub fn prepare_expired_fqdns_for_reap(ping_workers : &mut HashMap<String, PollThreadInfo>, expired_fqdns : &Vec<String>, reap_threads : &mut Vec<PollThreadInfo>) {
    for expired_fqdn in expired_fqdns.iter() {
        match ping_workers.remove(expired_fqdn) {
            Some(ping_worker) => {
                reap_threads.push(ping_worker);
            },
            None => {}
        }
    }
}

pub fn check_expired_fqdn_workers(devices : &HashMap<String, models::json::Device>, ping_workers : &HashMap<String, PollThreadInfo>, expired_fqdns : &mut Vec<String>) {
    for (fqdn, ping_worker) in ping_workers.iter() {
        match devices.get(fqdn) {
            Some(_) => {},
            None => {
                ping_worker.running.store(false, sync::atomic::Ordering::Relaxed);
                expired_fqdns.push(fqdn.clone());
            }
        }
    }
}