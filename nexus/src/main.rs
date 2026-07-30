#[macro_use] extern crate serde;
extern crate serde_json;
extern crate diesel;
#[macro_use] extern crate rocket;
extern crate config;
mod routes;
mod models;
mod db;
mod schema;
mod utilities;
mod snmp;
mod collectors;
mod mock;
mod traphandler;
use std::sync::{Arc, Mutex};
use std::sync::atomic::AtomicBool;
use std::collections::HashMap;
use config::{Config, File, Environment};

fn should_continue(running : &std::sync::atomic::AtomicBool) -> bool {
    return running.load(std::sync::atomic::Ordering::Relaxed);
}

fn refresh_imds_items(conn: &mut db::AnyConnection, imds: &Arc<Mutex<utilities::imds::IMDS>>) {
    let mut refresh_devices : Vec<models::dbo::Device> = Vec::new();
    let mut refresh_interfaces : HashMap<String, Vec<models::dbo::Interface>> = HashMap::new();
    for device in models::dbo::Device::monitored(conn).iter() {
        let device_fqdn = format!("{}.{}", device.name, device.dns_domain);
        refresh_devices.push(device.clone());
        refresh_interfaces.insert(device_fqdn, device.interfaces(conn));
    }
    {
        if let Ok(ref mut imds) = imds.lock() {
            // Purge devices deleted from the DB (API delete or state reset) so
            // their metrics stop being exported.
            let monitored_fqdns: std::collections::HashSet<String> = refresh_interfaces.keys().cloned().collect();
            imds.retain_devices(&monitored_fqdns);
            for device in refresh_devices.iter() {
                let device_fqdn = format!("{}.{}", device.name, device.dns_domain);
                imds.refresh_device(&device_fqdn, &device.base_mac);
                if let Some(device_interfaces) = refresh_interfaces.get(&device_fqdn) {
                    for interface in device_interfaces.iter() {
                        imds.refresh_interface(&device_fqdn, interface.index, &interface.interface_type, &interface.name(), interface.connected_interface.is_some() || interface.virtual_connection.is_some(), interface.speed_override);
                    }
                    // Drop interfaces whose ifindex is no longer in the DB set
                    // (reindexed or removed), so a stale ghost stops exporting a
                    // duplicate metric series. Mirrors retain_devices, per device.
                    let live_ifindexes: std::collections::HashSet<i32> = device_interfaces.iter().map(|i| i.index).collect();
                    imds.retain_interfaces(&device_fqdn, &live_ifindexes);
                }
            }
        }
    }
}

// Periodically re-derive fleet issues and fold them into the tracker so onset
// times / recurrence detection stay accurate independently of UI polling.
fn issue_scan_worker(
    running: Arc<AtomicBool>,
    imds: Arc<Mutex<utilities::imds::IMDS>>,
    entity_metrics: Arc<Mutex<collectors::entitypoller::EntityMetricsStore>>,
    lag_store: Arc<Mutex<collectors::lagpoller::LagStore>>,
    vlan_store: Arc<Mutex<collectors::vlanpoller::VlanStore>>,
    cache_controller: Arc<Mutex<utilities::cache::CacheController>>,
    tracker: Arc<Mutex<utilities::issues::IssueTracker>>,
) {
    let interval_secs: u64 = std::env::var("JASPY_ISSUE_SCAN_SECS").ok()
        .and_then(|v| v.parse().ok()).filter(|v| *v > 0).unwrap_or(15);
    println!("[issues] scanning for fleet issues every {}s", interval_secs);
    let pool = db::connect();
    let mut counter = 0u64;
    loop {
        if !should_continue(&running) { break; }
        if counter == 0 {
            match pool.get() {
                Ok(mut conn) => {
                    let derived = routes::api::v1::collect_issues(&mut *conn, &imds, &entity_metrics, &lag_store, &vlan_store, &cache_controller);
                    let now = utilities::tools::get_time_msecs();
                    let tracked = if let Ok(mut tracker) = tracker.lock() {
                        tracker.reconcile(now, derived)
                    } else {
                        Vec::new()
                    };
                    // Publish the fresh snapshot so concurrent /issues and
                    // per-device readers serve it instead of re-deriving.
                    if let Ok(cc) = cache_controller.lock() {
                        cc.store_issues(tracked, interval_secs as f64);
                    }
                }
                Err(e) => println!("[issues] failed to acquire db connection for scan: {}", e),
            }
        }
        counter = if counter + 1 >= interval_secs { 0 } else { counter + 1 };
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
}

fn imds_worker(running : Arc<AtomicBool>, imds : Arc<Mutex<utilities::imds::IMDS>>) {
    // Refresh the IMDS device/interface metadata from the DB every N seconds
    // (default 10). Configurable so tests can converge quickly.
    let refresh_secs: u64 = std::env::var("JASPY_IMDS_REFRESH_SECS").ok()
        .and_then(|v| v.parse().ok()).unwrap_or(10);
    println!("[imds] refreshing device metadata from db every {}s", refresh_secs);
    let refresh_threshold = refresh_secs.saturating_sub(1);
    let mut refresh_run_counter = 0;
    let pool = db::connect();
    loop {
        if !should_continue(&running) { break; }
        let mut refresh = false;
        if refresh_run_counter == 0 {
            refresh = true;
        }
        if refresh {
            match pool.get() {
                Ok(mut conn) => refresh_imds_items(&mut *conn, &imds),
                Err(e) => println!("[imds] failed to acquire db connection for refresh: {}", e),
            }
        }
        if refresh_run_counter >= refresh_threshold { refresh_run_counter = 0; } else { refresh_run_counter += 1; }
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
}

// Serialize the health store and write it atomically (temp file + rename) so a
// crash mid-write can't leave a truncated state file.
//
// Note: serialization happens under the IMDS lock. For the network sizes jaspy
// targets and a 60s cadence this is a negligible pause; if it ever matters,
// snapshot the store under the lock and serialize the clone outside it.
fn dump_health(imds: &Arc<Mutex<utilities::imds::IMDS>>, path: &str) {
    let json = match imds.lock() {
        Ok(imds) => match imds.health_to_json() {
            Ok(json) => json,
            Err(e) => { println!("[health] serialize failed: {}", e); return; }
        },
        Err(_) => return,
    };
    let tmp = format!("{}.tmp", path);
    if let Err(e) = std::fs::write(&tmp, json.as_bytes()) {
        println!("[health] write {} failed: {}", tmp, e);
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        println!("[health] rename to {} failed: {}", path, e);
    }
}

// Dump the health store to disk every 60s (plus once on shutdown, in main).
fn health_persist_worker(running: Arc<AtomicBool>, imds: Arc<Mutex<utilities::imds::IMDS>>, path: String) {
    println!("[health] persisting interface health state to {} every 60s", path);
    let mut counter = 0u64;
    loop {
        if !should_continue(&running) { break; }
        if counter >= 60 { dump_health(&imds, &path); counter = 0; } else { counter += 1; }
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
}

fn main() {
    // `jaspy-nexus trap-handler` (snmptrapd traphandle, formerly the
    // standalone jaspy-snmptrapd-reader binary) must run outside the tokio
    // runtime: it uses reqwest::blocking and fork().
    if std::env::args().nth(1).as_deref() == Some("trap-handler") {
        traphandler::run();
        return;
    }
    // `jaspy-nexus mock`: fake network + real collectors for local API/UI
    // development. prepare() must run before server_main reads env/spawns
    // threads; the guard's Drop stops the ephemeral postgres after rocket's
    // graceful ctrl-c shutdown returns from server_main.
    if std::env::args().nth(1).as_deref() == Some("mock") {
        let _guard = mock::prepare();
        rocket::execute(server_main());
        return;
    }
    rocket::execute(server_main());
}

async fn server_main() {
    println!("jaspy-nexus {} starting", env!("CARGO_PKG_VERSION"));
    let c = Config::builder()
        .add_source(File::with_name("/etc/jaspy/poller.yml").required(false))
        .add_source(File::with_name("~/.config/jaspy/poller.yml").required(false))
        .add_source(Environment::with_prefix("JASPY"))
        .build()
        .unwrap();

    // Configuration for the in-process poller/pinger collectors. Defaults match
    // the old standalone jaspy-poller/jaspy-pinger systemd units so a missing
    // value never panics (the standalone poller panicked on missing POLL_LOOP_MSECS).
    let snmpbot_url = c.get_string("snmpbot_url").unwrap_or_else(|_| "http://127.0.0.1:8286/".to_string());
    let poll_loop_msecs = c.get_int("poll_loop_msecs").unwrap_or(10000) as u64;
    let enable_poller = c.get_bool("enable_poller").unwrap_or(true);
    let enable_pinger = c.get_bool("enable_pinger").unwrap_or(true);

    // SNMP access mode: "snmpbot" (default; the external HTTP service) or
    // "embedded" (an in-process snmp2 v2c client + MIB registry). Selection is
    // config, not a build feature, so one binary serves both deployments.
    let snmp_mode = c.get_string("snmp_mode").unwrap_or_else(|_| "snmpbot".to_string());
    let snmp_timeout_ms = c.get_int("snmp_timeout_ms").unwrap_or(2000) as u64;
    let snmp_retries = c.get_int("snmp_retries").unwrap_or(2) as u32;
    let snmp_bulk_max_repetitions = c.get_int("snmp_bulk_max_repetitions").unwrap_or(20) as u32;
    // UDP port for the embedded client. Default 161; the perf harness overrides
    // it (JASPY_SNMP_PORT) so a simulated fleet can run unprivileged. Ignored
    // for devices whose fqdn already carries an explicit `:port`.
    let snmp_port = c.get_int("snmp_port").unwrap_or(161) as u16;
    // Global cap on concurrent SNMP requests across ALL collectors, protecting
    // the shared back end from burst overrun (PERF.md #4). 0 = unlimited
    // (historical behaviour).
    let snmp_max_inflight = c.get_int("snmp_max_inflight").unwrap_or(0).max(0) as usize;
    // Embedded-mode MIB directory: config override, else the nexus package
    // location, else the snmpbot package location.
    let snmp_mib_dir = c.get_string("snmp_mib_dir").ok().or_else(|| {
        ["/usr/share/jaspy/mibs", "/var/lib/snmpbot/mibs"]
            .iter()
            .find(|d| std::path::Path::new(d).is_dir())
            .map(|d| d.to_string())
    });

    let mut resolved_mib_dir: Option<String> = None;
    let mut snmp_mibs_loaded: Option<usize> = None;
    let snmp: Arc<snmp::SnmpSource> = match snmp_mode.as_str() {
        "snmpbot" => Arc::new(snmp::SnmpSource::new(
            snmp::SnmpBackend::SnmpbotHttp(snmp::snmpbot_http::SnmpbotHttp::new(snmpbot_url.clone())),
            snmp_max_inflight,
        )),
        "embedded" => {
            let dir = snmp_mib_dir.clone().unwrap_or_else(|| {
                panic!("[nexus] snmp_mode=embedded but no MIB directory found; set JASPY_SNMP_MIB_DIR")
            });
            let registry = snmp::mib::MibRegistry::load(std::path::Path::new(&dir))
                .unwrap_or_else(|e| panic!("[nexus] snmp_mode=embedded failed to load MIBs from {}: {}", dir, e));
            snmp_mibs_loaded = Some(registry.table_count());
            resolved_mib_dir = Some(dir);
            println!("[nexus] embedded SNMP client: {} tables / {} objects loaded from {}",
                registry.table_count(), registry.object_count(), resolved_mib_dir.as_deref().unwrap_or(""));
            let embedded = snmp::embedded::Embedded::new(
                Arc::new(registry),
                snmp_port,
                std::time::Duration::from_millis(snmp_timeout_ms),
                snmp_retries,
                snmp_bulk_max_repetitions,
            );
            Arc::new(snmp::SnmpSource::new(snmp::SnmpBackend::Embedded(embedded), snmp_max_inflight))
        }
        other => panic!("[nexus] unknown snmp_mode '{}' (expected 'snmpbot' or 'embedded')", other),
    };

    // entitypoller collector (formerly the standalone jaspy-entitypoller binary):
    // entity sensors + per-VLAN STP, default poll interval 120s (matches the Go
    // -poll-interval default).
    let enable_entitypoller = c.get_bool("enable_entitypoller").unwrap_or(true);
    let entitypoller_interval_msecs = c.get_int("entitypoller_interval_msecs").unwrap_or(120000) as u64;
    let entitypoller_disable_sensors = c.get_bool("entitypoller_disable_sensors").unwrap_or(false);
    let entitypoller_disable_stp = c.get_bool("entitypoller_disable_stp").unwrap_or(false);

    // vlanpoller collector: per-interface VLAN membership (native + tagged)
    // into an in-memory store, default poll interval 5 minutes. Also pollable
    // on demand per device via POST /api/v1/devices/<fqdn>/vlans/poll.
    let enable_vlanpoller = c.get_bool("enable_vlanpoller").unwrap_or(true);
    let vlanpoller_interval_msecs = c.get_int("vlanpoller_interval_msecs").unwrap_or(300000) as u64;

    // lagpoller collector: port-channel membership + LACP health into an
    // in-memory store, default poll interval 5 minutes.
    let enable_lagpoller = c.get_bool("enable_lagpoller").unwrap_or(true);
    let lagpoller_interval_msecs = c.get_int("lagpoller_interval_msecs").unwrap_or(300000) as u64;

    // Embedded SNMP trap receiver (independent of snmp_mode so traps can be
    // migrated off snmptrapd separately).
    let enable_trap_receiver = c.get_bool("enable_trap_receiver").unwrap_or(false);
    let trap_bind_address = c.get_string("trap_bind_address").unwrap_or_else(|_| "0.0.0.0:162".to_string());

    // Discovery engine (formerly the standalone Python `discover` tool). The
    // in-memory config is seeded from JASPY_DISCOVERY_* env vars and mutable
    // via PUT /dev/discovery/config; setting an interval enables periodic runs.
    let discovery_interval_secs = c.get_int("discovery_interval_secs").ok().map(|v| v as u64);
    let split_csv = |v: Result<String, config::ConfigError>| -> Vec<String> {
        v.map(|s| s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()).unwrap_or_default()
    };
    let discovery_config_env_seed = models::json::DiscoveryConfig {
        root_device: c.get_string("discovery_root_device").ok(),
        community: c.get_string("discovery_community").ok(),
        dns_domains: split_csv(c.get_string("discovery_dns_domains")),
        ignore: split_csv(c.get_string("discovery_ignore")),
        remap: split_csv(c.get_string("discovery_remap")).iter()
            .filter_map(|entry| entry.split_once(':').map(|(s, d)| (s.to_string(), d.to_string())))
            .collect(),
        topology_stable: c.get_bool("discovery_stable").unwrap_or(true),
        periodic_enabled: discovery_interval_secs.is_some(),
        interval_secs: discovery_interval_secs.unwrap_or(3600),
    };

    // Apply pending embedded migrations for the configured backend before any
    // DB read (the discovery-config load below needs the settings table).
    // Opt-out: JASPY_AUTO_MIGRATE=false.
    db::auto_migrate();

    // A config persisted via PUT /dev/discovery/config (settings table) wins
    // over the env seed; env is only the first-boot default. Log which source
    // is used so an ignored env edit or an unreadable persisted config is
    // visible instead of silent.
    let pool = db::connect();
    let discovery_config = match pool.get() {
        Ok(mut conn) => match models::dbo::Setting::get(&mut *conn, "discovery_config") {
            Some(json) => match serde_json::from_str::<models::json::DiscoveryConfig>(&json) {
                Ok(config) => {
                    println!("[discovery] using persisted config from database (JASPY_DISCOVERY_* env is only the first-boot seed)");
                    config
                },
                Err(e) => {
                    println!("[discovery] failed to parse persisted config ({}), falling back to env seed", e);
                    discovery_config_env_seed
                }
            },
            None => {
                println!("[discovery] no persisted config, using JASPY_DISCOVERY_* env seed");
                discovery_config_env_seed
            }
        },
        Err(e) => {
            println!("[discovery] could not load persisted config (db unavailable: {}), using env seed", e);
            discovery_config_env_seed
        }
    };

    // Per-interface health signal store (utilities::health): windows and
    // thresholds for the discards/errors/flapping/speed/utilization badges.
    // Defaults match utilities::health::HealthConfig::default().
    let mut health_cfg = utilities::health::HealthConfig::default();
    if let Ok(v) = c.get_int("health_counter_window_secs") { health_cfg.counter_window_ms = (v as u64) * 1000; }
    if let Ok(v) = c.get_int("health_flap_window_secs") { health_cfg.flap_window_ms = (v as u64) * 1000; }
    if let Ok(v) = c.get_int("health_util_window_secs") { health_cfg.util_window_ms = (v as u64) * 1000; }
    if let Ok(v) = c.get_int("health_throughput_window_secs") { health_cfg.throughput_window_ms = (v as u64) * 1000; }
    if let Ok(v) = c.get_float("health_util_threshold_pct") { health_cfg.util_threshold_pct = v; }
    if let Ok(v) = c.get_int("health_stale_secs") { health_cfg.stale_ms = (v as u64) * 1000; }
    if let Ok(v) = c.get_int("health_error_threshold") { health_cfg.error_show_threshold = v as u64; }
    if let Ok(v) = c.get_int("health_discard_threshold") { health_cfg.discard_show_threshold = v as u64; }
    if let Ok(v) = c.get_int("health_flap_threshold") { health_cfg.flap_show_threshold = v as u32; }
    // Optional disk persistence: unset = disabled (no filesystem side effects).
    let health_state_path = c.get_string("health_state_path").ok().filter(|p| !p.is_empty());

    let running = Arc::new(AtomicBool::new(true));
    let msgbus : Arc<Mutex<utilities::msgbus::MessageBus>> = Arc::new(Mutex::new(utilities::msgbus::MessageBus::new()));
    let imds : Arc<Mutex<utilities::imds::IMDS>> = Arc::new(Mutex::new(utilities::imds::IMDS::new(msgbus.clone(), health_cfg)));
    // Reload persisted health state before collectors start writing.
    if let Some(path) = health_state_path.as_ref() {
        match std::fs::read_to_string(path) {
            Ok(json) => match imds.lock() {
                Ok(mut imds) => match imds.load_health_json(&json) {
                    Ok(()) => println!("[health] reloaded interface health state from {}", path),
                    Err(e) => println!("[health] failed to parse state file {}: {}", path, e),
                },
                Err(e) => println!("[health] could not lock IMDS to reload state: {}", e),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                println!("[health] no state file at {} yet (first run)", path);
            }
            Err(e) => println!("[health] could not read state file {}: {}", path, e),
        }
    }
    let entity_metrics : Arc<Mutex<collectors::entitypoller::EntityMetricsStore>> = Arc::new(Mutex::new(collectors::entitypoller::EntityMetricsStore::new()));
    // Managed unconditionally (like entity_metrics) so the /api/v1 routes work
    // even when the collector thread is disabled — they just serve empty data.
    let vlan_store : Arc<Mutex<collectors::vlanpoller::VlanStore>> = Arc::new(Mutex::new(collectors::vlanpoller::VlanStore::new()));
    let lag_store : Arc<Mutex<collectors::lagpoller::LagStore>> = Arc::new(Mutex::new(collectors::lagpoller::LagStore::new()));
    let vlan_control : Arc<Mutex<collectors::vlanpoller::VlanPollerControl>> = Arc::new(Mutex::new(collectors::vlanpoller::VlanPollerControl::new()));
    let cache_controller : Arc<Mutex<utilities::cache::CacheController>> = Arc::new(Mutex::new(utilities::cache::CacheController::new()));
    // Derived-issue onset tracker (utilities::issues). Grace keeps a briefly
    // cleared issue's first_seen stable across a single missed scan; a genuine
    // clear-then-recur gets a fresh first_seen so a stale ack re-alerts.
    let issue_tracker : Arc<Mutex<utilities::issues::IssueTracker>> = Arc::new(Mutex::new(utilities::issues::IssueTracker::new(60_000)));
    // Seed first_seen from persisted acknowledgements before any scan runs, so a
    // restart keeps a still-active acked issue acknowledged (a genuine
    // clear-then-recur, detected at runtime, still re-alerts). Must precede the
    // scan thread below.
    match pool.get() {
        Ok(mut conn) => {
            if let Ok(mut tracker) = issue_tracker.lock() {
                let now = utilities::tools::get_time_msecs();
                for ack in models::dbo::IssueAck::all(&mut *conn) {
                    tracker.seed(ack.issue_key, ack.first_seen as u64, now);
                }
            }
        }
        Err(e) => println!("[issues] could not seed tracker from persisted acks: {}", e),
    }
    // Keep the tracker warm even when nobody is viewing the Issues page, so
    // onset times and recurrence detection stay accurate.
    let issue_scan_thread = {
        let running_collector = running.clone();
        let imds_collector = imds.clone();
        let entity_collector = entity_metrics.clone();
        let lag_collector = lag_store.clone();
        let vlan_collector = vlan_store.clone();
        let cache_collector = cache_controller.clone();
        let tracker_collector = issue_tracker.clone();
        std::thread::spawn(move || {
            issue_scan_worker(running_collector, imds_collector, entity_collector, lag_collector, vlan_collector, cache_collector, tracker_collector);
        })
    };

    let imds_worker_imds = imds.clone();
    let imds_worker_running = running.clone();
    let imds_worker_thread = std::thread::spawn(|| {
        imds_worker(imds_worker_running, imds_worker_imds);
    });

    // In-process collectors (formerly the jaspy-poller and jaspy-pinger
    // binaries). They report directly into IMDS rather than PUTing over HTTP.
    let poller_thread = if enable_poller {
        let imds_collector = imds.clone();
        let running_collector = running.clone();
        let snmp_collector = snmp.clone();
        // Without the pinger, the poller doubles as the device up/down source
        // (a device answering SNMP is up).
        let report_device_status = !enable_pinger;
        Some(std::thread::spawn(move || {
            collectors::poller::run(snmp_collector, poll_loop_msecs, report_device_status, imds_collector, running_collector);
        }))
    } else {
        println!("[poller] disabled via JASPY_ENABLE_POLLER");
        None
    };

    let pinger_thread = if enable_pinger {
        let imds_collector = imds.clone();
        let running_collector = running.clone();
        Some(std::thread::spawn(move || {
            collectors::pinger::run(imds_collector, running_collector);
        }))
    } else {
        println!("[pinger] disabled via JASPY_ENABLE_PINGER");
        None
    };

    let entitypoller_thread = if enable_entitypoller {
        let store_collector = entity_metrics.clone();
        let running_collector = running.clone();
        let snmp_collector = snmp.clone();
        Some(std::thread::spawn(move || {
            collectors::entitypoller::run(snmp_collector, entitypoller_interval_msecs, entitypoller_disable_sensors, entitypoller_disable_stp, store_collector, running_collector);
        }))
    } else {
        println!("[entitypoller] disabled via JASPY_ENABLE_ENTITYPOLLER");
        None
    };

    let vlanpoller_thread = if enable_vlanpoller {
        let store_collector = vlan_store.clone();
        let control_collector = vlan_control.clone();
        let running_collector = running.clone();
        let snmp_collector = snmp.clone();
        Some(std::thread::spawn(move || {
            collectors::vlanpoller::run(snmp_collector, vlanpoller_interval_msecs, control_collector, store_collector, running_collector);
        }))
    } else {
        println!("[vlanpoller] disabled via JASPY_ENABLE_VLANPOLLER");
        None
    };

    let lagpoller_thread = if enable_lagpoller {
        let store_collector = lag_store.clone();
        let running_collector = running.clone();
        let snmp_collector = snmp.clone();
        Some(std::thread::spawn(move || {
            collectors::lagpoller::run(snmp_collector, lagpoller_interval_msecs, store_collector, running_collector);
        }))
    } else {
        println!("[lagpoller] disabled via JASPY_ENABLE_LAGPOLLER");
        None
    };

    let discovery_control: Arc<Mutex<collectors::discovery::DiscoveryControl>> =
        Arc::new(Mutex::new(collectors::discovery::DiscoveryControl::new(discovery_config)));
    let discovery_thread = {
        let control = discovery_control.clone();
        let msgbus_collector = msgbus.clone();
        let running_collector = running.clone();
        let snmp_collector = snmp.clone();
        let cache_collector = cache_controller.clone();
        std::thread::spawn(move || {
            collectors::discovery::run(snmp_collector, control, msgbus_collector, cache_collector, running_collector);
        })
    };

    let trap_receiver_thread = if enable_trap_receiver {
        let imds_collector = imds.clone();
        let running_collector = running.clone();
        let bind = trap_bind_address.clone();
        Some(std::thread::spawn(move || {
            snmp::trap::run(bind, imds_collector, running_collector);
        }))
    } else {
        None
    };

    let health_persist_thread = health_state_path.as_ref().map(|path| {
        let imds_collector = imds.clone();
        let running_collector = running.clone();
        let path = path.clone();
        std::thread::spawn(move || health_persist_worker(running_collector, imds_collector, path))
    });

    let runtime_info : Arc<Mutex<models::internal::RuntimeInfo>> = Arc::new(Mutex::new(models::internal::RuntimeInfo::new()));

    // Weathermap statics dir (mounted below when present); resolved here so
    // the system status endpoint can report it either way.
    let weathermap_dir = std::env::var("JASPY_WEATHERMAP_DIR").unwrap_or_else(|_| "/var/lib/jaspy/weathermap".to_string());
    let weathermap_dir_present = std::path::Path::new(&weathermap_dir).is_dir();

    // Megaexcel integration base URL (e.g. https://megaexcel.arenius.fi). When
    // set, the web UI shows Megaexcel links in the nav and on each device page;
    // when unset/blank the integration is hidden entirely. Trailing slash is
    // trimmed so the UI can append paths cleanly.
    let megaexcel_url = std::env::var("JASPY_MEGAEXCEL_URL").ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty());

    // Effective feature configuration for GET /api/v1/system.
    let system_info = models::internal::SystemInfo {
        snmpbot_url: snmpbot_url.clone(),
        snmp_mode: snmp_mode.clone(),
        snmp_mib_dir: resolved_mib_dir.clone(),
        snmp_mibs_loaded: snmp_mibs_loaded,
        trap_receiver_enabled: enable_trap_receiver,
        trap_bind_address: if enable_trap_receiver { Some(trap_bind_address.clone()) } else { None },
        poller_enabled: enable_poller,
        poll_loop_msecs: poll_loop_msecs,
        pinger_enabled: enable_pinger,
        entitypoller_enabled: enable_entitypoller,
        entitypoller_interval_msecs: entitypoller_interval_msecs,
        entitypoller_sensors_enabled: !entitypoller_disable_sensors,
        entitypoller_stp_enabled: !entitypoller_disable_stp,
        vlanpoller_enabled: enable_vlanpoller,
        vlanpoller_interval_msecs: vlanpoller_interval_msecs,
        lagpoller_enabled: enable_lagpoller,
        lagpoller_interval_msecs: lagpoller_interval_msecs,
        weathermap_dir: if weathermap_dir_present { Some(weathermap_dir.clone()) } else { None },
        megaexcel_url,
        db_url: db::redacted_db_url(&std::env::var("JASPY_DB_URL").unwrap_or_default()),
        db_backend: db::backend_kind(&std::env::var("JASPY_DB_URL").unwrap_or_default()).as_str().to_string(),
    };

    let mut rocket_app = rocket::build()
        .mount(
            "/dev/device",
            routes![
                routes::dev::device::list,
                routes::dev::device::get_device,
                routes::dev::device::create,
                routes::dev::device::update,
                routes::dev::device::delete,
                routes::dev::device::interfaces,
                routes::dev::device::monitored_device_list,
                routes::dev::device::monitored_device_report,
                routes::dev::device::device_status,
                routes::dev::device::device_interface_status,
                routes::dev::device::clear_device_connection,
            ]
        )
        .mount(
            "/dev/clientlocation",
            routes![
                routes::dev::clientlocation::get_clientlocation,
                routes::dev::clientlocation::put_clientlocation,
            ]
        )
        .mount(
            "/dev/discovery",
            routes![
                routes::dev::discovery::discovery_device,
                routes::dev::discovery::discovery_links,
                routes::dev::discovery::discovery_run,
                routes::dev::discovery::discovery_status,
                routes::dev::discovery::discovery_get_config,
                routes::dev::discovery::discovery_put_config,
            ]
        )
        .mount(
            "/dev/interface",
            routes![
                routes::dev::interface::interface_list,
                routes::dev::interface::interface_monitor_report,
            ]
        )
        .mount(
            "/dev/metrics",
            routes![
                routes::dev::metrics::metrics_fast,
                routes::dev::metrics::metrics,
                routes::dev::metrics::metrics_perf,
            ]
        )
        .mount(
            "/dev/weathermap",
            routes![
                routes::dev::weathermap::full_topology_data,
                routes::dev::weathermap::state_information,
                routes::dev::weathermap::get_position_data,
                routes::dev::weathermap::put_position_data,
            ]
        )
        // UI-facing API: the single prefix that will go behind auth later.
        .mount(
            "/api/v1",
            routes![
                routes::api::v1::summary,
                routes::api::v1::devices,
                routes::api::v1::device_detail,
                routes::api::v1::device_entity,
                routes::api::v1::device_vlan_poll,
                routes::api::v1::vlans,
                routes::api::v1::stp_summary,
                routes::api::v1::stp_tree,
                routes::api::v1::stp_expected_root_add,
                routes::api::v1::stp_expected_root_remove,
                routes::api::v1::issues,
                routes::api::v1::issue_ack,
                routes::api::v1::issue_unack,
                routes::api::v1::issue_types,
                routes::api::v1::issue_suppress,
                routes::api::v1::issue_unsuppress,
                routes::api::v1::device_create,
                routes::api::v1::device_update,
                routes::api::v1::device_delete,
                routes::api::v1::clientlocations,
                routes::api::v1::event_get,
                routes::api::v1::event_put,
                routes::api::v1::reset,
                routes::api::v1::system_status,
                routes::api::v1::system_perf,
                routes::api::v1::system_env,
                routes::api::v1::ws_logs,
            ]
        )
        // Discovery control re-mounted for the UI: same handlers as /dev/discovery.
        .mount(
            "/api/v1/discovery",
            routes![
                routes::dev::discovery::discovery_run,
                routes::dev::discovery::discovery_status,
                routes::dev::discovery::discovery_get_config,
                routes::dev::discovery::discovery_put_config,
            ]
        )
        // Embedded React admin UI with SPA fallback (lowest rank catch-all).
        .mount("/", routes![routes::webui::spa])
        .manage(pool.clone())
        .manage(imds.clone())
        .manage(entity_metrics.clone())
        .manage(vlan_store.clone())
        .manage(lag_store.clone())
        .manage(vlan_control.clone())
        .manage(discovery_control.clone())
        .manage(cache_controller.clone())
        .manage(issue_tracker.clone())
        .manage(runtime_info.clone())
        .manage(system_info)
        .manage(msgbus.clone())
        // gzip/brotli-compress responses per the request's Accept-Encoding.
        // Attached last so it sees the final body of every route (UI + API).
        .attach(rocket_async_compression::Compression::fairing());

    // Serve the existing PIXI weathermap statics when present (replaces the
    // apache2 DocumentRoot; config.js can now use relative /dev/weathermap).
    if weathermap_dir_present {
        rocket_app = rocket_app.mount("/weathermap", rocket::fs::FileServer::from(weathermap_dir));
    }

    // A failed launch (e.g. port already in use by another nexus instance)
    // must be loud, not silently fall through to shutdown.
    if let Err(e) = rocket_app.launch().await {
        println!("[nexus] failed to launch http server: {}", e);
    }

    (*running).store(false, std::sync::atomic::Ordering::Relaxed);
    imds_worker_thread.join().unwrap();
    if let Some(poller_thread) = poller_thread { let _ = poller_thread.join(); }
    if let Some(pinger_thread) = pinger_thread { let _ = pinger_thread.join(); }
    if let Some(entitypoller_thread) = entitypoller_thread { let _ = entitypoller_thread.join(); }
    if let Some(vlanpoller_thread) = vlanpoller_thread { let _ = vlanpoller_thread.join(); }
    if let Some(lagpoller_thread) = lagpoller_thread { let _ = lagpoller_thread.join(); }
    if let Some(trap_receiver_thread) = trap_receiver_thread { let _ = trap_receiver_thread.join(); }
    let _ = discovery_thread.join();
    let _ = issue_scan_thread.join();
    if let Some(health_persist_thread) = health_persist_thread { let _ = health_persist_thread.join(); }
    // Final dump so a graceful shutdown never loses the last minute of state.
    if let Some(path) = health_state_path.as_ref() { dump_health(&imds, path); }
}
