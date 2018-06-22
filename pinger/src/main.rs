#[macro_use]
extern crate serde_derive;
extern crate serde_json;
extern crate reqwest;
extern crate oping;

use std::collections::HashMap;
use std::thread;
use std::time;
use std::sync;
use std::time::{SystemTime, UNIX_EPOCH};


const MAIN_LOOP_MSECS : u64 = 1000;
const PING_LOOP_MSECS : u64 = 1000;
const PING_TIMEOUT : f64 = 1.0;
const PING_HYST_LOOP_MSECS : u64 = 100;
const PING_HYST_LIMIT : u8 = 10;

struct PingThreadInfo {
    thd : thread::JoinHandle<()>,
    running : sync::Arc<sync::atomic::AtomicBool>,
    finished_signal : sync::mpsc::Receiver<bool>
}

struct PingAccountingInfo {
    responsive : bool,
    hysteresis_responsive : u8,
    hysteresis_unresponsive : u8
}

#[derive(Deserialize,Debug)]
struct DeviceInfo {
    fqdn : String,
    responsive : bool
}

#[derive(Deserialize,Debug)]
struct DeviceInfoResponse {
    devices : Vec<DeviceInfo>
}

fn get_devices(source_url : &String) -> Result<HashMap<String, DeviceInfo>, String> {
    let mut devices : HashMap<String, DeviceInfo> = HashMap::new();
    let response = reqwest::get(source_url);
    match response {
        Ok(mut response) => {
            let resp_json : Result<DeviceInfoResponse, _> = response.json();
            match resp_json {
                Ok(device_info_response) => {
                    for device_info in device_info_response.devices.iter() {
                        devices.insert(
                            device_info.fqdn.clone(),
                            DeviceInfo {
                                fqdn: device_info.fqdn.clone(),
                                responsive: device_info.responsive
                            }
                        );
                    }
                    return Ok(devices);
                },
                Err(_) => {
                    return Err("Unable to parse DeviceInfoResponse JSON".to_string())
                }
            }
        },
        Err(e) => {
            let status_code = e.status();
            let error = format!("HTTP Error (status={:?}) while trying to get DeviceInfoResponse", status_code);
            return Err(error);
        }
    }
}

fn pinger_prepare_instance(host : &String) -> Result<oping::Ping, oping::PingError> {
    let mut oping_instance = oping::Ping::new();
    match oping_instance.set_timeout(PING_TIMEOUT) {
        Ok(_) => {},
        Err(e) => {
            return Err(e);
        }
    }
    match oping_instance.add_host(host.as_str()) {
        Ok(_) => {},
        Err(e) => {
            return Err(e);
        }
    }
    return Ok(oping_instance);
}

fn pinger_handle_host_drop(host : &String, ping_accounting_info : &mut PingAccountingInfo) {
    if !ping_accounting_info.responsive {
        ping_accounting_info.hysteresis_responsive = 0;
        ping_accounting_info.hysteresis_unresponsive = 0;
        return;
    } else {
        ping_accounting_info.hysteresis_responsive = 0;
    }
    ping_accounting_info.hysteresis_unresponsive += 1;
    if ping_accounting_info.hysteresis_unresponsive >= PING_HYST_LIMIT {
        ping_accounting_info.responsive = false;
        println!("[{}] -> DOWN", host);
    } else {
        println!("[{}] <hyst> not responding ({}/{})", host, ping_accounting_info.hysteresis_unresponsive, PING_HYST_LIMIT);
    }
}

fn pinger_handle_host_resp(host : &String, ping_accounting_info : &mut PingAccountingInfo) {
    if ping_accounting_info.responsive {
        ping_accounting_info.hysteresis_responsive = 0;
        ping_accounting_info.hysteresis_unresponsive = 0;
        return;
    } else {
        ping_accounting_info.hysteresis_unresponsive = 0;
    }
    ping_accounting_info.hysteresis_responsive += 1;
    if ping_accounting_info.hysteresis_responsive >= PING_HYST_LIMIT {
        ping_accounting_info.responsive = true;
        println!("[{}] -> OK", host);
    } else {
        println!("[{}] <hyst> responding ({}/{})", host, ping_accounting_info.hysteresis_responsive, PING_HYST_LIMIT);
    }
}

fn is_responding(ping_item : &oping::PingItem) -> bool {
    if ping_item.dropped > 0 || ping_item.latency_ms < 0.0 { return false; }
    return true;
}

fn pinger_process_ping_result(host : &String, mut ping_accounting_info : &mut PingAccountingInfo, ping_item : oping::PingItem) {
    if is_responding(&ping_item) {
        pinger_handle_host_resp(host, &mut ping_accounting_info);
    } else {
        pinger_handle_host_drop(host, &mut ping_accounting_info);
    }
}

fn pinger_perform_ping(host : &String, mut ping_accounting_info : &mut PingAccountingInfo, oping_instance : oping::Ping) {
    match oping_instance.send() {
        Ok(oping_result) => {
            match oping_result.last() {
                Some(ping_result) => {
                    pinger_process_ping_result(host, &mut ping_accounting_info, ping_result);
                },
                None => {}
            }
        },
        Err(e) => {
            println!("[{}] ping error: {:?}", host, e);
        }
    }
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

fn pinger(device_info : DeviceInfo, running : sync::Arc<sync::atomic::AtomicBool>, done : sync::mpsc::Sender<bool>) {
    let mut ping_accounting_info : PingAccountingInfo = PingAccountingInfo {
        responsive: device_info.responsive,
        hysteresis_responsive: 0,
        hysteresis_unresponsive: 0
    };
    println!("[{}] start monitoring", device_info.fqdn);
    while running.load(std::sync::atomic::Ordering::Relaxed) {
        let start = get_time_msecs();
        
        match pinger_prepare_instance(&device_info.fqdn) {
            Ok(oping_instance) => {
                pinger_perform_ping(&device_info.fqdn, &mut ping_accounting_info, oping_instance);
            },
            Err(e) => {
                println!("[{}] ping instance creation error: {:?}", device_info.fqdn, e);
            }
        }

        let diff = get_time_msecs() - start;
        let mut loop_time = PING_LOOP_MSECS;
        if ping_accounting_info.hysteresis_responsive > 0 || ping_accounting_info.hysteresis_unresponsive > 0 {
            loop_time = PING_HYST_LOOP_MSECS;
        }
        if diff <= loop_time {
            thread::sleep(time::Duration::from_millis(loop_time - diff));
        }
    }
    done.send(true).unwrap();
    println!("[{}] stop monitoring", device_info.fqdn);
}

fn start_ping_worker(device_info : &DeviceInfo, ping_workers : &mut HashMap<String, PingThreadInfo>) {
    let running = std::sync::Arc::new(sync::atomic::AtomicBool::new(true));
    let running_ptit = running.clone();
    let (tx, rx) = sync::mpsc::channel();
    let device_info_copy = DeviceInfo {
        responsive: device_info.responsive,
        fqdn: device_info.fqdn.clone()
    };
    ping_workers.insert(
        device_info.fqdn.clone(), 
        PingThreadInfo {
            thd: thread::spawn(|| {
                pinger(device_info_copy, running_ptit, tx)
            }),
            running: running,
            finished_signal: rx
        }
    );
}

fn reap_finished_threads(reap_threads : &mut Vec<PingThreadInfo>) {
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
            let reaped_pti : PingThreadInfo = reap_threads.swap_remove(idx);
            reaped_pti.thd.join().unwrap();
        } else {
            break;
        }
    }
}

fn prepare_expired_fqdns_for_reap(ping_workers : &mut HashMap<String, PingThreadInfo>, expired_fqdns : &Vec<String>, reap_threads : &mut Vec<PingThreadInfo>) {
    for expired_fqdn in expired_fqdns.iter() {
        match ping_workers.remove(expired_fqdn) {
            Some(ping_worker) => {
                reap_threads.push(ping_worker);
            },
            None => {}
        }
    }
}

fn check_expired_fqdn_workers(devices : &HashMap<String, DeviceInfo>, ping_workers : &HashMap<String, PingThreadInfo>, expired_fqdns : &mut Vec<String>) {
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

fn check_if_worker_needed(devices : &HashMap<String, DeviceInfo>, mut ping_workers : &mut HashMap<String, PingThreadInfo>) {
    for (fqdn, device_info) in devices.iter() {
        if ping_workers.contains_key(&*fqdn) {
            continue;
        }
        start_ping_worker(&device_info, &mut ping_workers);
    }
}

fn main() {
    let source_url;
    let mut ping_workers : HashMap<String, PingThreadInfo> = HashMap::new();
    let mut reap_threads : Vec<PingThreadInfo> = Vec::new();
    let mut devices : HashMap<String, DeviceInfo> = HashMap::new();

    if let Some(argv1) = std::env::args().nth(1) {
        source_url = argv1;
    } else {
        println!("usage: {} <device_source_url>", std::env::args().nth(0).unwrap());
        return;
    }
    
    loop {
        match get_devices(&source_url) {
            Ok(received_devices) => {
                devices = received_devices;
            },
            Err(error) => {
                println!("Failed to get new device listing: {}", error);
            }
        }
        let mut expired_fqdns : Vec<String> = Vec::new();
        check_if_worker_needed(&devices, &mut ping_workers);
        check_expired_fqdn_workers(&devices, &ping_workers, &mut expired_fqdns);
        prepare_expired_fqdns_for_reap(&mut ping_workers, &expired_fqdns, &mut reap_threads);
        reap_finished_threads(&mut reap_threads);
        thread::sleep(time::Duration::from_millis(MAIN_LOOP_MSECS));
    }
}
