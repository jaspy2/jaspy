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
mod collectors;
use std::sync::{Arc, Mutex};
use std::sync::atomic::AtomicBool;
use std::collections::HashMap;
use config::{Config, File, Environment};

fn should_continue(running : &std::sync::atomic::AtomicBool) -> bool {
    return running.load(std::sync::atomic::Ordering::Relaxed);
}

fn refresh_imds_items(conn: &mut diesel::PgConnection, imds: &Arc<Mutex<utilities::imds::IMDS>>) {
    let mut refresh_devices : Vec<models::dbo::Device> = Vec::new();
    let mut refresh_interfaces : HashMap<String, Vec<models::dbo::Interface>> = HashMap::new();
    for device in models::dbo::Device::monitored(conn).iter() {
        let device_fqdn = format!("{}.{}", device.name, device.dns_domain);
        refresh_devices.push(device.clone());
        refresh_interfaces.insert(device_fqdn, device.interfaces(conn));
    }
    {
        if let Ok(ref mut imds) = imds.lock() {
            for device in refresh_devices.iter() {
                let device_fqdn = format!("{}.{}", device.name, device.dns_domain);
                imds.refresh_device(&device_fqdn);
                if let Some(device_interfaces) = refresh_interfaces.get(&device_fqdn) {
                    for interface in device_interfaces.iter() {
                        imds.refresh_interface(&device_fqdn, interface.index, &interface.interface_type, &interface.name(), interface.connected_interface.is_some() || interface.virtual_connection.is_some(), interface.speed_override);
                    }
                }
            }
        }
    }
}

fn imds_worker(running : Arc<AtomicBool>, imds : Arc<Mutex<utilities::imds::IMDS>>) {
    // Refresh the IMDS device/interface metadata from the DB every N seconds
    // (default 10). Configurable so tests can converge quickly.
    let refresh_secs: u64 = std::env::var("JASPY_IMDS_REFRESH_SECS").ok()
        .and_then(|v| v.parse().ok()).unwrap_or(10);
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
            if let Ok(mut conn) = pool.get() {
                refresh_imds_items(&mut *conn, &imds);
            } else {
                // TODO: log
            }
        }
        if refresh_run_counter >= refresh_threshold { refresh_run_counter = 0; } else { refresh_run_counter += 1; }
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
}

#[rocket::main]
async fn main() {
    let mut c = Config::new();

    c.merge(File::with_name("/etc/jaspy/poller.yml").required(false)).unwrap()
        .merge(File::with_name("~/.config/jaspy/poller.yml").required(false)).unwrap()
        .merge(Environment::with_prefix("JASPY")).unwrap();

    // Configuration for the in-process poller/pinger collectors. Defaults match
    // the old standalone jaspy-poller/jaspy-pinger systemd units so a missing
    // value never panics (the standalone poller panicked on missing POLL_LOOP_MSECS).
    let snmpbot_url = c.get_str("snmpbot_url").unwrap_or_else(|_| "http://127.0.0.1:8286/".to_string());
    let poll_loop_msecs = c.get_int("poll_loop_msecs").unwrap_or(10000) as u64;
    let enable_poller = c.get_bool("enable_poller").unwrap_or(true);
    let enable_pinger = c.get_bool("enable_pinger").unwrap_or(true);

    // entitypoller collector (formerly the standalone jaspy-entitypoller binary):
    // entity sensors + per-VLAN STP, default poll interval 120s (matches the Go
    // -poll-interval default).
    let enable_entitypoller = c.get_bool("enable_entitypoller").unwrap_or(true);
    let entitypoller_interval_msecs = c.get_int("entitypoller_interval_msecs").unwrap_or(120000) as u64;
    let entitypoller_disable_sensors = c.get_bool("entitypoller_disable_sensors").unwrap_or(false);
    let entitypoller_disable_stp = c.get_bool("entitypoller_disable_stp").unwrap_or(false);

    // Discovery engine (formerly the standalone Python `discover` tool). The
    // in-memory config is seeded from JASPY_DISCOVERY_* env vars and mutable
    // via PUT /dev/discovery/config; setting an interval enables periodic runs.
    let discovery_interval_secs = c.get_int("discovery_interval_secs").ok().map(|v| v as u64);
    let split_csv = |v: Result<String, config::ConfigError>| -> Vec<String> {
        v.map(|s| s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()).unwrap_or_default()
    };
    let discovery_config = models::json::DiscoveryConfig {
        root_device: c.get_str("discovery_root_device").ok(),
        community: c.get_str("discovery_community").ok(),
        dns_domains: split_csv(c.get_str("discovery_dns_domains")),
        ignore: split_csv(c.get_str("discovery_ignore")),
        remap: split_csv(c.get_str("discovery_remap")).iter()
            .filter_map(|entry| entry.split_once(':').map(|(s, d)| (s.to_string(), d.to_string())))
            .collect(),
        topology_stable: c.get_bool("discovery_stable").unwrap_or(true),
        periodic_enabled: discovery_interval_secs.is_some(),
        interval_secs: discovery_interval_secs.unwrap_or(3600),
    };

    let running = Arc::new(AtomicBool::new(true));
    let msgbus : Arc<Mutex<utilities::msgbus::MessageBus>> = Arc::new(Mutex::new(utilities::msgbus::MessageBus::new()));
    let imds : Arc<Mutex<utilities::imds::IMDS>> = Arc::new(Mutex::new(utilities::imds::IMDS::new(msgbus.clone())));
    let entity_metrics : Arc<Mutex<collectors::entitypoller::EntityMetricsStore>> = Arc::new(Mutex::new(collectors::entitypoller::EntityMetricsStore::new()));
    let cache_controller : Arc<Mutex<utilities::cache::CacheController>> = Arc::new(Mutex::new(utilities::cache::CacheController::new()));

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
        let snmpbot_url_collector = snmpbot_url.clone();
        Some(std::thread::spawn(move || {
            collectors::poller::run(snmpbot_url_collector, poll_loop_msecs, imds_collector, running_collector);
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
        let snmpbot_url_collector = snmpbot_url.clone();
        Some(std::thread::spawn(move || {
            collectors::entitypoller::run(snmpbot_url_collector, entitypoller_interval_msecs, entitypoller_disable_sensors, entitypoller_disable_stp, store_collector, running_collector);
        }))
    } else {
        println!("[entitypoller] disabled via JASPY_ENABLE_ENTITYPOLLER");
        None
    };

    let discovery_control: Arc<Mutex<collectors::discovery::DiscoveryControl>> =
        Arc::new(Mutex::new(collectors::discovery::DiscoveryControl::new(discovery_config)));
    let discovery_thread = {
        let control = discovery_control.clone();
        let msgbus_collector = msgbus.clone();
        let running_collector = running.clone();
        let snmpbot_url_collector = snmpbot_url.clone();
        let cache_collector = cache_controller.clone();
        std::thread::spawn(move || {
            collectors::discovery::run(snmpbot_url_collector, control, msgbus_collector, cache_collector, running_collector);
        })
    };

    let runtime_info : Arc<Mutex<models::internal::RuntimeInfo>> = Arc::new(Mutex::new(models::internal::RuntimeInfo::new()));

    let _ = rocket::build()
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
        .manage(db::connect())
        .manage(imds.clone())
        .manage(entity_metrics.clone())
        .manage(discovery_control.clone())
        .manage(cache_controller.clone())
        .manage(runtime_info.clone())
        .manage(msgbus.clone())
        .launch()
        .await;

    (*running).store(false, std::sync::atomic::Ordering::Relaxed);
    imds_worker_thread.join().unwrap();
    if let Some(poller_thread) = poller_thread { let _ = poller_thread.join(); }
    if let Some(pinger_thread) = pinger_thread { let _ = pinger_thread.join(); }
    if let Some(entitypoller_thread) = entitypoller_thread { let _ = entitypoller_thread.join(); }
    let _ = discovery_thread.join();
}
