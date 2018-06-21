use std::collections::HashMap;
use std::collections::HashSet;
use std::thread;
use std::time;
use std::sync;

const MAIN_LOOP_MSECS : u64 = 1000;
const PING_LOOP_MSECS : u64 = 1000;

struct PingThreadInfo {
    thd : thread::JoinHandle<()>,
    running : sync::Arc<sync::atomic::AtomicBool>,
    finished_signal : sync::mpsc::Receiver<bool>
}

fn get_hosts() -> HashSet<String> {
    static mut retd : u32 = 0;
    let mut hosts : HashSet<String> = HashSet::new();
    unsafe {
    if retd == 0 {
        hosts.insert(String::from("172.24.2.1"));
        retd += 1;
    }
    }
    return hosts;
}

fn pinger(host : String, running : sync::Arc<sync::atomic::AtomicBool>, done : sync::mpsc::Sender<bool>) {
    println!("[{}] start monitoring", host);
    while running.load(std::sync::atomic::Ordering::Relaxed) {
        thread::sleep(time::Duration::from_millis(PING_LOOP_MSECS));
    }
    done.send(true).unwrap();
    println!("[{}] stop monitoring", host);
}

fn start_ping_worker(host : &String, ping_workers : &mut HashMap<String, PingThreadInfo>) {
    let running = std::sync::Arc::new(sync::atomic::AtomicBool::new(true));
    let running_ptit = running.clone();
    let host_ptit = host.clone();
    let (tx, rx) = sync::mpsc::channel();
    ping_workers.insert(
        host.clone(), 
        PingThreadInfo {
            thd: thread::spawn(|| {
                pinger(host_ptit, running_ptit, tx)
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

fn check_expired_fqdn_workers(hosts : &HashSet<String>, ping_workers : &HashMap<String, PingThreadInfo>, expired_fqdns : &mut Vec<String>) {
    for (fqdn, ping_worker) in ping_workers.iter() {
        match hosts.get(fqdn) {
            Some(_) => {},
            None => {
                ping_worker.running.store(false, sync::atomic::Ordering::Relaxed);
                expired_fqdns.push(fqdn.clone());
            }
        }
    }
}

fn check_if_worker_needed(hosts : &HashSet<String>, mut ping_workers : &mut HashMap<String, PingThreadInfo>) {
    for host in hosts.iter() {
        if ping_workers.contains_key(&*host) {
            continue;
        }
        start_ping_worker(&host, &mut ping_workers);
    }
}

fn main() {
    let mut ping_workers : HashMap<String, PingThreadInfo> = HashMap::new();
    let mut reap_threads : Vec<PingThreadInfo> = Vec::new();
    loop {
        let hosts = get_hosts();
        let mut expired_fqdns : Vec<String> = Vec::new();
        check_if_worker_needed(&hosts, &mut ping_workers);
        check_expired_fqdn_workers(&hosts, &ping_workers, &mut expired_fqdns);
        prepare_expired_fqdns_for_reap(&mut ping_workers, &expired_fqdns, &mut reap_threads);
        reap_finished_threads(&mut reap_threads);
        thread::sleep(time::Duration::from_millis(MAIN_LOOP_MSECS));
    }
}
