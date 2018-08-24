#[macro_use] extern crate serde_derive;
extern crate reqwest;
extern crate rand as crate_rand;
use crate_rand::prelude::*;
mod models;
use std::time;
use std::time::{SystemTime, UNIX_EPOCH};
use std::sync::{Arc, mpsc, atomic};
use std::collections::HashMap;
use std::thread;
use std::sync;


const MAIN_LOOP_MSECS : u64 = 1000;
const POLL_LOOP_MSECS : u64 = 30000;

pub struct PollThreadInfo {
    pub thd : thread::JoinHandle<()>,
    pub running : Arc<atomic::AtomicBool>,
    pub finished_signal : mpsc::Receiver<bool>
}

fn print_usage() {
    println!("usage: {} <jaspy_url> <snmpbot_url>", std::env::args().nth(0).unwrap());
}

fn get_time_msecs() -> u64 {
    let start = SystemTime::now();
    match start.duration_since(UNIX_EPOCH) {
        Ok(unix_time) => {
            let in_ms = unix_time.as_secs() * 1000 + unix_time.subsec_nanos() as u64 / 1_000_000;
            return in_ms;
        },
        Err(_) => { return 0; }
    }
}

fn get_devices(jaspy_url : &String) -> Result<HashMap<String, models::json::Device>, String> {
    let source_url = format!("{}/device", jaspy_url);
    let mut devices : HashMap<String, models::json::Device> = HashMap::new();
    if let Ok(mut response) = reqwest::get(&source_url) {
        let resp_json : Result<Vec<models::json::Device>, _> = response.json();
        if let Ok(device_list) = resp_json {
            for device_info in device_list.iter() {
                if let Some(polling_enabled) = device_info.polling_enabled {
                    if !polling_enabled {
                        continue;
                    }
                }
                let fqdn = format!("{}.{}", device_info.name, device_info.dns_domain);
                devices.insert(fqdn, device_info.clone());
            }
        } else {
            return Err("failed to parse JSON from jaspy".to_string());
        }
    } else {
        return Err("failed to get device information from jaspy".to_string());
    }

    return Ok(devices);
}

fn poller(jaspy_url : String, snmpbot_url: String, device_info : models::json::Device, running : sync::Arc<sync::atomic::AtomicBool>, done : sync::mpsc::Sender<bool>) {
    let fqdn = format!("{}.{}", device_info.name, device_info.dns_domain);
    println!("[{}] start polling", fqdn);
    let start_sleep = crate_rand::prelude::thread_rng().gen_range(0.0, POLL_LOOP_MSECS as f64);
    thread::sleep(time::Duration::from_millis(start_sleep as u64));
    while running.load(std::sync::atomic::Ordering::Relaxed) {
        let start = get_time_msecs();

        println!("[{}] polling", fqdn);

        let diff = get_time_msecs() - start;
        let loop_time = POLL_LOOP_MSECS;
        if diff <= loop_time {
            thread::sleep(time::Duration::from_millis(loop_time - diff));
        }
    }
    done.send(true).unwrap();
    println!("[{}] stop polling", fqdn);
}

fn start_poll_worker(jaspy_url: &String, snmpbot_url: &String, device_info : &models::json::Device, ping_workers : &mut HashMap<String, PollThreadInfo>) {
    let running = std::sync::Arc::new(sync::atomic::AtomicBool::new(true));
    let running_ptit = running.clone();
    let (tx, rx) = sync::mpsc::channel();
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

fn check_if_worker_needed(jaspy_url : &String, snmpbot_url: &String, devices : &HashMap<String, models::json::Device>, mut ping_workers : &mut HashMap<String, PollThreadInfo>) {
    for (fqdn, device_info) in devices.iter() {
        if ping_workers.contains_key(&*fqdn) {
            continue;
        }
        start_poll_worker(jaspy_url, snmpbot_url, &device_info, &mut ping_workers);
    }
}

fn reap_finished_threads(reap_threads : &mut Vec<PollThreadInfo>) {
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
                        std::sync::mpsc::TryRecvError::Empty => {
                            idx += 1;
                        },
                        std::sync::mpsc::TryRecvError::Disconnected => {
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

fn prepare_expired_fqdns_for_reap(ping_workers : &mut HashMap<String, PollThreadInfo>, expired_fqdns : &Vec<String>, reap_threads : &mut Vec<PollThreadInfo>) {
    for expired_fqdn in expired_fqdns.iter() {
        match ping_workers.remove(expired_fqdn) {
            Some(ping_worker) => {
                reap_threads.push(ping_worker);
            },
            None => {}
        }
    }
}

fn check_expired_fqdn_workers(devices : &HashMap<String, models::json::Device>, ping_workers : &HashMap<String, PollThreadInfo>, expired_fqdns : &mut Vec<String>) {
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

fn main() {
    let jaspy_url;
    let snmpbot_url;
    let mut poll_workers : HashMap<String, PollThreadInfo> = HashMap::new();
    let mut reap_threads : Vec<PollThreadInfo> = Vec::new();
    let mut devices : HashMap<String, models::json::Device> = HashMap::new();


    if let Some(argv1) = std::env::args().nth(1) {
        jaspy_url = argv1;
    } else {
        print_usage();
        return;
    }

    if let Some(argv2) = std::env::args().nth(2) {
        snmpbot_url = argv2;
    } else {
        print_usage();
        return;
    }
    
    loop {
        match get_devices(&jaspy_url) {
            Ok(received_devices) => {
                devices = received_devices;
            },
            Err(error) => {
                println!("Failed to get new device listing: {}", error);
            }
        }
        let mut expired_fqdns : Vec<String> = Vec::new();
        check_if_worker_needed(&jaspy_url, &snmpbot_url, &devices, &mut poll_workers);
        check_expired_fqdn_workers(&devices, &poll_workers, &mut expired_fqdns);
        prepare_expired_fqdns_for_reap(&mut poll_workers, &expired_fqdns, &mut reap_threads);
        reap_finished_threads(&mut reap_threads);
        thread::sleep(time::Duration::from_millis(MAIN_LOOP_MSECS));
    }
}