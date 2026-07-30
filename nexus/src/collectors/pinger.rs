// In-process ICMP up/down collector (formerly the `jaspy-pinger` binary).
// Per-device reachability changes are written directly into IMDS
// (`report_device`). Requires CAP_NET_RAW.
//
// The fleet is pinged by a small pool of `workers` shard threads
// (JASPY_PINGER_WORKERS, default 4) rather than one thread per device. Each
// worker owns a disjoint shard of devices (by `shard_of`) and, once per tick,
// pings its whole shard from a SINGLE liboping instance — liboping shares one
// socket-pair per address family across all hosts added to an instance, so a
// shard of N hosts costs ~1 socket, not N. This replaces the old model that
// built a fresh instance (a raw ICMP socket + DNS lookup) per device per second
// across ~200 threads, which exhausted file descriptors under load
// ("ping instance creation error: Too many open files"). Workers start staggered
// so their sends spread across the interval instead of firing as one burst.
extern crate oping;

use crate::models;
use crate::db;
use crate::utilities::imds::IMDS;
use crate::utilities::tools;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, atomic};
use std::thread;
use std::time;

const PING_LOOP_MSECS: u64 = 1000;
const PING_TIMEOUT: f64 = 1.0;
const PING_HYST_LOOP_MSECS: u64 = 100;
const PING_HYST_LIMIT: u8 = 10;

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

fn is_responding(ping_item: &oping::PingItem) -> bool {
    if ping_item.dropped > 0 || ping_item.latency_ms < 0.0 { return false; }
    return true;
}

// Which worker owns a device. Stable within a run (DefaultHasher), so a device
// stays on the same worker as the monitored set changes.
fn shard_of(fqdn: &str, workers: usize) -> usize {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    fqdn.hash(&mut hasher);
    (hasher.finish() % workers as u64) as usize
}

// Pure hysteresis step, replacing the old handle_host_resp/handle_host_drop pair.
// Returns Some(new_up) when a reachability transition should be reported, else
// None. Semantics preserved verbatim: the first observation of an unknown device
// is reported immediately; otherwise a device only flips after PING_HYST_LIMIT
// consecutive contrary observations, and any matching observation resets the
// counters.
fn advance_hysteresis(info: &mut PingAccountingInfo, responding: bool) -> Option<bool> {
    match info.responsive {
        None => {
            info.responsive = Some(responding);
            Some(responding)
        }
        Some(current) if current == responding => {
            info.hysteresis_responsive = 0;
            info.hysteresis_unresponsive = 0;
            None
        }
        Some(_) => {
            if responding {
                info.hysteresis_unresponsive = 0;
                info.hysteresis_responsive = info.hysteresis_responsive.saturating_add(1);
                if info.hysteresis_responsive >= PING_HYST_LIMIT {
                    info.responsive = Some(true);
                    return Some(true);
                }
            } else {
                info.hysteresis_responsive = 0;
                info.hysteresis_unresponsive = info.hysteresis_unresponsive.saturating_add(1);
                if info.hysteresis_unresponsive >= PING_HYST_LIMIT {
                    info.responsive = Some(false);
                    return Some(false);
                }
            }
            None
        }
    }
}

// Bring the accounting map in line with the current shard membership: drop
// departed devices, and seed newcomers from IMDS's last-known `up` (matching the
// old per-worker startup seed) so a restart doesn't re-announce every device.
fn reconcile_accounting(accounting: &mut HashMap<String, PingAccountingInfo>, shard: &[String], imds: &Arc<Mutex<IMDS>>) {
    let current: HashSet<&String> = shard.iter().collect();
    accounting.retain(|fqdn, _| current.contains(fqdn));
    for fqdn in shard {
        if !accounting.contains_key(fqdn) {
            let initial_up = if let Ok(ref imds_locked) = imds.lock() {
                imds_locked.get_device(fqdn).and_then(|d| d.up)
            } else {
                None
            };
            accounting.insert(
                fqdn.clone(),
                PingAccountingInfo { responsive: initial_up, hysteresis_responsive: 0, hysteresis_unresponsive: 0 },
            );
        }
    }
}

// Apply this tick's ping results to the accounting map and return the transitions
// to report. A shard device absent from `responded` (add_host failed, or no
// reply row) counts as not responding. Pure — unit-tested without sockets/IMDS.
fn advance_shard(accounting: &mut HashMap<String, PingAccountingInfo>, shard: &[String], responded: &HashMap<String, bool>) -> Vec<(String, bool)> {
    let mut reports = Vec::new();
    for fqdn in shard {
        let responding = responded.get(fqdn).copied().unwrap_or(false);
        if let Some(info) = accounting.get_mut(fqdn) {
            if let Some(new_up) = advance_hysteresis(info, responding) {
                reports.push((fqdn.clone(), new_up));
            }
        }
    }
    reports
}

// Build a liboping instance with every shard host added (best-effort: a host
// whose add_host fails — e.g. unresolvable — is simply omitted and will read as
// not-responding this cycle). Returns the instance and the number of hosts added.
fn build_shard_instance(shard: &[String]) -> Result<(oping::Ping, usize), oping::PingError> {
    let mut ping = oping::Ping::new();
    ping.set_timeout(PING_TIMEOUT)?;
    let mut added = 0usize;
    for fqdn in shard {
        if ping.add_host(fqdn.as_str()).is_ok() {
            added += 1;
        }
    }
    Ok((ping, added))
}

fn ping_shard_worker(pool: db::Pool, imds: Arc<Mutex<IMDS>>, shard_id: usize, workers: usize, running: Arc<atomic::AtomicBool>) {
    // Stagger workers across the interval so their sends don't align into one
    // burst — with `workers` shards there are pings continuously in flight.
    let offset = (shard_id as u64) * PING_LOOP_MSECS / (workers as u64);
    if offset > 0 {
        thread::sleep(time::Duration::from_millis(offset));
    }
    println!("[pinger] shard {}/{} start monitoring", shard_id, workers);
    let mut accounting: HashMap<String, PingAccountingInfo> = HashMap::new();

    while running.load(atomic::Ordering::Relaxed) {
        let start = tools::get_time_msecs();

        let shard: Vec<String> = load_device_fqdns(&pool)
            .into_keys()
            .filter(|fqdn| shard_of(fqdn, workers) == shard_id)
            .collect();
        reconcile_accounting(&mut accounting, &shard, &imds);

        if !shard.is_empty() {
            match build_shard_instance(&shard) {
                Ok((ping, added)) if added > 0 => match ping.send() {
                    Ok(results) => {
                        let mut responded: HashMap<String, bool> = HashMap::new();
                        for item in results {
                            responded.insert(item.hostname.clone(), is_responding(&item));
                        }
                        for (fqdn, up) in advance_shard(&mut accounting, &shard, &responded) {
                            report_up(&pool, &imds, &fqdn, up);
                            println!("[{}] -> {}", fqdn, if up { "OK" } else { "DOWN" });
                        }
                    }
                    // A local send failure (not a device-down signal) must not flap
                    // the whole shard: log and leave accounting untouched this cycle.
                    Err(e) => println!("[pinger] shard {} ping send error: {:?}", shard_id, e),
                },
                // Nothing resolved this cycle: skip the send.
                Ok(_) => {}
                Err(e) => println!("[pinger] shard {} ping instance creation error: {:?}", shard_id, e),
            }
        }

        // Tick faster while any shard device is mid-hysteresis (confirming a flip).
        let in_hyst = accounting.values().any(|i| i.hysteresis_responsive > 0 || i.hysteresis_unresponsive > 0);
        let loop_time = if in_hyst { PING_HYST_LOOP_MSECS } else { PING_LOOP_MSECS };
        let diff = tools::get_time_msecs() - start;
        if diff < loop_time {
            thread::sleep(time::Duration::from_millis(loop_time - diff));
        }
    }
    println!("[pinger] shard {}/{} stop monitoring", shard_id, workers);
}

pub fn run(imds: Arc<Mutex<IMDS>>, running: Arc<atomic::AtomicBool>, workers: usize) {
    let workers = workers.max(1);
    println!("[pinger] starting in-process collector ({} shard workers)", workers);
    let pool = db::connect();

    let mut handles = Vec::new();
    for shard_id in 0..workers {
        let pool_copy = pool.clone();
        let imds_copy = imds.clone();
        let running_copy = running.clone();
        handles.push(thread::spawn(move || {
            ping_shard_worker(pool_copy, imds_copy, shard_id, workers, running_copy);
        }));
    }

    // Workers exit their loops when `running` clears; join them on shutdown.
    for handle in handles {
        let _ = handle.join();
    }
    println!("[pinger] collector stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(responsive: Option<bool>) -> PingAccountingInfo {
        PingAccountingInfo { responsive, hysteresis_responsive: 0, hysteresis_unresponsive: 0 }
    }

    #[test]
    fn hysteresis_reports_initial_state_once() {
        let mut i = info(None);
        assert_eq!(advance_hysteresis(&mut i, true), Some(true)); // first observation reported
        assert_eq!(i.responsive, Some(true));
        assert_eq!(advance_hysteresis(&mut i, true), None); // stable afterwards
        let mut d = info(None);
        assert_eq!(advance_hysteresis(&mut d, false), Some(false));
    }

    #[test]
    fn hysteresis_stable_state_is_noop_and_resets_counters() {
        let mut i = info(Some(true));
        i.hysteresis_unresponsive = 4; // pretend a few misses accumulated
        assert_eq!(advance_hysteresis(&mut i, true), None);
        assert_eq!(i.hysteresis_unresponsive, 0);
        assert_eq!(i.hysteresis_responsive, 0);
    }

    #[test]
    fn hysteresis_flips_exactly_at_limit() {
        let mut i = info(Some(true));
        for _ in 0..(PING_HYST_LIMIT - 1) {
            assert_eq!(advance_hysteresis(&mut i, false), None); // still up, counting
        }
        assert_eq!(advance_hysteresis(&mut i, false), Some(false)); // flips on the LIMITth miss
        assert_eq!(i.responsive, Some(false));
    }

    #[test]
    fn hysteresis_bounce_resets_pending_flip() {
        let mut i = info(Some(true));
        for _ in 0..5 {
            advance_hysteresis(&mut i, false);
        }
        assert_eq!(i.hysteresis_unresponsive, 5);
        assert_eq!(advance_hysteresis(&mut i, true), None); // one recovery cancels the pending down
        assert_eq!(i.hysteresis_unresponsive, 0);
    }

    #[test]
    fn shard_of_is_deterministic_and_in_range() {
        let fqdns = ["a.example.com", "b.example.com", "c.example.com", "org-sw7.asm.fi", "x"];
        for w in [1usize, 2, 4, 8] {
            for f in fqdns {
                let s = shard_of(f, w);
                assert!(s < w, "{} shard {} out of range for {} workers", f, s, w);
                assert_eq!(s, shard_of(f, w), "shard_of must be deterministic");
            }
        }
    }

    #[test]
    fn shard_of_spreads_across_workers() {
        let workers = 4;
        let mut seen = HashSet::new();
        for n in 0..200 {
            seen.insert(shard_of(&format!("dev{}.example.com", n), workers));
        }
        assert_eq!(seen.len(), workers, "a 200-device sample should hit every shard");
    }

    #[test]
    fn advance_shard_missing_host_counts_as_not_responding() {
        let mut acc = HashMap::new();
        acc.insert("up.example.com".to_string(), info(Some(true)));
        let shard = vec!["up.example.com".to_string()];
        let responded: HashMap<String, bool> = HashMap::new(); // no reply row => down
        let reports = advance_shard(&mut acc, &shard, &responded);
        assert!(reports.is_empty()); // one miss doesn't flip yet
        assert_eq!(acc["up.example.com"].hysteresis_unresponsive, 1);
    }

    #[test]
    fn advance_shard_reports_new_device_up() {
        let mut acc = HashMap::new();
        acc.insert("new.example.com".to_string(), info(None));
        let shard = vec!["new.example.com".to_string()];
        let mut responded = HashMap::new();
        responded.insert("new.example.com".to_string(), true);
        let reports = advance_shard(&mut acc, &shard, &responded);
        assert_eq!(reports, vec![("new.example.com".to_string(), true)]);
    }
}
