// In-process ICMP up/down collector (formerly the `jaspy-pinger` binary).
// Ported near-verbatim from pinger/src/main.rs; per-device state changes are
// written directly into IMDS (`report_device`) rather than PUT to
// /dev/device/monitor. The cross-process `state_id` resync is dropped (it only
// existed to detect a nexus restart from a separate process); worker lifetime
// now simply tracks the monitored device set. Requires CAP_NET_RAW.
extern crate oping;

use crate::models;
use crate::db;
use crate::utilities::imds::IMDS;
use crate::utilities::tools;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, atomic, mpsc};
use std::thread;
use std::time;

const PING_LOOP_MSECS: u64 = 1000;
const PING_TIMEOUT: f64 = 1.0;
const PING_HYST_LOOP_MSECS: u64 = 100;
const PING_HYST_LIMIT: u8 = 10;

struct PingThreadInfo {
    thd: thread::JoinHandle<()>,
    running: Arc<atomic::AtomicBool>,
    finished_signal: mpsc::Receiver<bool>,
}

struct PingAccountingInfo {
    responsive: Option<bool>,
    hysteresis_responsive: u8,
    hysteresis_unresponsive: u8,
}

fn load_device_fqdns(pool: &db::Pool) -> HashMap<String, ()> {
    let mut devices: HashMap<String, ()> = HashMap::new();
    if let Ok(mut conn) = pool.get() {
        for device in models::dbo::Device::monitored(&mut *conn).iter() {
            let fqdn = format!("{}.{}", device.name, device.dns_domain);
            devices.insert(fqdn, ());
        }
    } else {
        println!("[pinger] failed to acquire db connection for device listing");
    }
    return devices;
}

fn report_up(pool: &db::Pool, imds: &Arc<Mutex<IMDS>>, host: &String, up: bool) {
    if let Ok(mut conn) = pool.get() {
        if let Ok(ref mut imds) = imds.lock() {
            imds.report_device(&mut *conn, models::json::DeviceMonitorReport { fqdn: host.clone(), up: up });
        }
    }
}

fn pinger_prepare_instance(host: &String) -> Result<oping::Ping, oping::PingError> {
    let mut oping_instance = oping::Ping::new();
    oping_instance.set_timeout(PING_TIMEOUT)?;
    oping_instance.add_host(host.as_str())?;
    return Ok(oping_instance);
}

fn pinger_handle_host_drop(pool: &db::Pool, imds: &Arc<Mutex<IMDS>>, host: &String, ping_accounting_info: &mut PingAccountingInfo) {
    let responsive;
    match ping_accounting_info.responsive {
        Some(value) => { responsive = value; },
        None => {
            // Initial state is down!
            ping_accounting_info.responsive = Some(false);
            report_up(pool, imds, host, false);
            return;
        }
    }
    if !responsive {
        ping_accounting_info.hysteresis_responsive = 0;
        ping_accounting_info.hysteresis_unresponsive = 0;
        return;
    } else {
        ping_accounting_info.hysteresis_responsive = 0;
    }
    ping_accounting_info.hysteresis_unresponsive += 1;
    if ping_accounting_info.hysteresis_unresponsive >= PING_HYST_LIMIT {
        ping_accounting_info.responsive = Some(false);
        report_up(pool, imds, host, false);
        println!("[{}] -> DOWN", host);
    } else {
        println!("[{}] <hyst> not responding ({}/{})", host, ping_accounting_info.hysteresis_unresponsive, PING_HYST_LIMIT);
    }
}

fn pinger_handle_host_resp(pool: &db::Pool, imds: &Arc<Mutex<IMDS>>, host: &String, ping_accounting_info: &mut PingAccountingInfo) {
    let responsive;
    match ping_accounting_info.responsive {
        Some(value) => { responsive = value; },
        None => {
            // Initial state is up!
            ping_accounting_info.responsive = Some(true);
            report_up(pool, imds, host, true);
            return;
        }
    }
    if responsive {
        ping_accounting_info.hysteresis_responsive = 0;
        ping_accounting_info.hysteresis_unresponsive = 0;
        return;
    } else {
        ping_accounting_info.hysteresis_unresponsive = 0;
    }
    ping_accounting_info.hysteresis_responsive += 1;
    if ping_accounting_info.hysteresis_responsive >= PING_HYST_LIMIT {
        ping_accounting_info.responsive = Some(true);
        report_up(pool, imds, host, true);
        println!("[{}] -> OK", host);
    } else {
        println!("[{}] <hyst> responding ({}/{})", host, ping_accounting_info.hysteresis_responsive, PING_HYST_LIMIT);
    }
}

fn is_responding(ping_item: &oping::PingItem) -> bool {
    if ping_item.dropped > 0 || ping_item.latency_ms < 0.0 { return false; }
    return true;
}

fn pinger_process_ping_result(pool: &db::Pool, imds: &Arc<Mutex<IMDS>>, host: &String, ping_accounting_info: &mut PingAccountingInfo, ping_item: oping::PingItem) {
    if is_responding(&ping_item) {
        pinger_handle_host_resp(pool, imds, host, ping_accounting_info);
    } else {
        pinger_handle_host_drop(pool, imds, host, ping_accounting_info);
    }
}

fn pinger_perform_ping(pool: &db::Pool, imds: &Arc<Mutex<IMDS>>, host: &String, ping_accounting_info: &mut PingAccountingInfo, oping_instance: oping::Ping) {
    match oping_instance.send() {
        Ok(oping_result) => {
            if let Some(ping_result) = oping_result.last() {
                pinger_process_ping_result(pool, imds, host, ping_accounting_info, ping_result);
            }
        },
        Err(e) => {
            println!("[{}] ping error: {:?}", host, e);
        }
    }
}

fn ping_worker(pool: db::Pool, imds: Arc<Mutex<IMDS>>, fqdn: String, initial_up: Option<bool>, running: Arc<atomic::AtomicBool>, done: mpsc::Sender<bool>) {
    let mut ping_accounting_info = PingAccountingInfo {
        responsive: initial_up,
        hysteresis_responsive: 0,
        hysteresis_unresponsive: 0,
    };
    println!("[{}] start monitoring", fqdn);
    while running.load(atomic::Ordering::Relaxed) {
        let start = tools::get_time_msecs();

        match pinger_prepare_instance(&fqdn) {
            Ok(oping_instance) => {
                pinger_perform_ping(&pool, &imds, &fqdn, &mut ping_accounting_info, oping_instance);
            },
            Err(e) => {
                println!("[{}] ping instance creation error: {:?}", fqdn, e);
            }
        }

        let diff = tools::get_time_msecs() - start;
        let mut loop_time = PING_LOOP_MSECS;
        if ping_accounting_info.hysteresis_responsive > 0 || ping_accounting_info.hysteresis_unresponsive > 0 {
            loop_time = PING_HYST_LOOP_MSECS;
        }
        if diff <= loop_time {
            thread::sleep(time::Duration::from_millis(loop_time - diff));
        }
    }
    let _ = done.send(true);
    println!("[{}] stop monitoring", fqdn);
}

fn check_if_worker_needed(pool: &db::Pool, imds: &Arc<Mutex<IMDS>>, devices: &HashMap<String, ()>, ping_workers: &mut HashMap<String, PingThreadInfo>) {
    for (fqdn, _) in devices.iter() {
        if ping_workers.contains_key(fqdn) {
            continue;
        }
        // Seed the initial reachability state from IMDS, matching the old
        // pinger which read `up` from GET /dev/device/monitor.
        let initial_up = if let Ok(ref imds_locked) = imds.lock() {
            imds_locked.get_device(fqdn).and_then(|d| d.up)
        } else {
            None
        };
        let worker_running = Arc::new(atomic::AtomicBool::new(true));
        let running_worker = worker_running.clone();
        let (tx, rx) = mpsc::channel();
        let pool_copy = pool.clone();
        let imds_copy = imds.clone();
        let fqdn_copy = fqdn.clone();
        ping_workers.insert(
            fqdn.clone(),
            PingThreadInfo {
                thd: thread::spawn(move || {
                    ping_worker(pool_copy, imds_copy, fqdn_copy, initial_up, running_worker, tx);
                }),
                running: worker_running,
                finished_signal: rx,
            },
        );
    }
}

fn check_expired_fqdn_workers(devices: &HashMap<String, ()>, ping_workers: &HashMap<String, PingThreadInfo>, expired_fqdns: &mut Vec<String>) {
    for (fqdn, ping_worker) in ping_workers.iter() {
        if devices.get(fqdn).is_none() {
            ping_worker.running.store(false, atomic::Ordering::Relaxed);
            expired_fqdns.push(fqdn.clone());
        }
    }
}

fn prepare_expired_fqdns_for_reap(ping_workers: &mut HashMap<String, PingThreadInfo>, expired_fqdns: &Vec<String>, reap_threads: &mut Vec<PingThreadInfo>) {
    for expired_fqdn in expired_fqdns.iter() {
        if let Some(ping_worker) = ping_workers.remove(expired_fqdn) {
            reap_threads.push(ping_worker);
        }
    }
}

fn reap_finished_threads(reap_threads: &mut Vec<PingThreadInfo>) {
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

pub fn run(imds: Arc<Mutex<IMDS>>, running: Arc<atomic::AtomicBool>) {
    println!("[pinger] starting in-process collector");
    let pool = db::connect();
    let mut ping_workers: HashMap<String, PingThreadInfo> = HashMap::new();
    let mut reap_threads: Vec<PingThreadInfo> = Vec::new();

    while running.load(atomic::Ordering::Relaxed) {
        let devices = load_device_fqdns(&pool);
        let mut expired_fqdns: Vec<String> = Vec::new();
        check_if_worker_needed(&pool, &imds, &devices, &mut ping_workers);
        check_expired_fqdn_workers(&devices, &ping_workers, &mut expired_fqdns);
        prepare_expired_fqdns_for_reap(&mut ping_workers, &expired_fqdns, &mut reap_threads);
        reap_finished_threads(&mut reap_threads);
        thread::sleep(time::Duration::from_millis(1000));
    }

    // Graceful shutdown: signal and join every worker.
    for (_fqdn, worker) in ping_workers.iter() {
        worker.running.store(false, atomic::Ordering::Relaxed);
    }
    for (_fqdn, worker) in ping_workers.drain() {
        let _ = worker.thd.join();
    }
    for worker in reap_threads.drain(..) {
        let _ = worker.thd.join();
    }
    println!("[pinger] collector stopped");
}
