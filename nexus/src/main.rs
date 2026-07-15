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
mod mock;
mod traphandler;
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
            // Purge devices deleted from the DB (API delete or state reset) so
            // their metrics stop being exported.
            let monitored_fqdns: std::collections::HashSet<String> = refresh_interfaces.keys().cloned().collect();
            imds.retain_devices(&monitored_fqdns);
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
        // Without the pinger, the poller doubles as the device up/down source
        // (a device answering SNMP is up).
        let report_device_status = !enable_pinger;
        Some(std::thread::spawn(move || {
            collectors::poller::run(snmpbot_url_collector, poll_loop_msecs, report_device_status, imds_collector, running_collector);
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

    // Weathermap statics dir (mounted below when present); resolved here so
    // the system status endpoint can report it either way.
    let weathermap_dir = std::env::var("JASPY_WEATHERMAP_DIR").unwrap_or_else(|_| "/var/lib/jaspy/weathermap".to_string());
    let weathermap_dir_present = std::path::Path::new(&weathermap_dir).is_dir();

    // Effective feature configuration for GET /api/v1/system.
    let system_info = models::internal::SystemInfo {
        snmpbot_url: snmpbot_url.clone(),
        poller_enabled: enable_poller,
        poll_loop_msecs: poll_loop_msecs,
        pinger_enabled: enable_pinger,
        entitypoller_enabled: enable_entitypoller,
        entitypoller_interval_msecs: entitypoller_interval_msecs,
        entitypoller_sensors_enabled: !entitypoller_disable_sensors,
        entitypoller_stp_enabled: !entitypoller_disable_stp,
        weathermap_dir: if weathermap_dir_present { Some(weathermap_dir.clone()) } else { None },
        db_url: db::redacted_db_url(&std::env::var("JASPY_DB_URL").unwrap_or_default()),
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
                routes::api::v1::device_create,
                routes::api::v1::device_update,
                routes::api::v1::device_delete,
                routes::api::v1::clientlocations,
                routes::api::v1::event_get,
                routes::api::v1::event_put,
                routes::api::v1::reset,
                routes::api::v1::system_status,
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
        .manage(discovery_control.clone())
        .manage(cache_controller.clone())
        .manage(runtime_info.clone())
        .manage(system_info)
        .manage(msgbus.clone());

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
    let _ = discovery_thread.join();
}
