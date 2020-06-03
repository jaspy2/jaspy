#![feature(plugin)]
#![feature(proc_macro_hygiene)]
#![feature(decl_macro)]
#[macro_use] extern crate serde;
extern crate serde_json;
#[macro_use] extern crate diesel;
#[macro_use] extern crate rocket;
#[macro_use] extern crate rocket_contrib;
extern crate r2d2;
extern crate r2d2_diesel;
extern crate time;
extern crate config;
extern crate rumq_client;
extern crate tokio;
mod routes;
mod models;
mod db;
mod schema;
mod utilities;
use std::sync::{Arc, Mutex};
use std::sync::atomic::AtomicBool;
use std::collections::HashMap;
use config::{Config, File, Environment};

fn should_continue(running : &std::sync::atomic::AtomicBool) -> bool {
    return running.load(std::sync::atomic::Ordering::Relaxed);
}

fn refresh_imds_items(conn: &diesel::PgConnection, imds: &Arc<Mutex<utilities::imds::IMDS>>) {
    let mut refresh_devices : Vec<models::dbo::Device> = Vec::new();
    let mut refresh_interfaces : HashMap<String, Vec<models::dbo::Interface>> = HashMap::new();
    for device in models::dbo::Device::monitored(&conn).iter() {
        let device_fqdn = format!("{}.{}", device.name, device.dns_domain);
        refresh_devices.push(device.clone());
        refresh_interfaces.insert(device_fqdn, device.interfaces(&conn));
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
    let mut refresh_run_counter = 0;
    let pool = db::connect();
    loop {
        if !should_continue(&running) { break; }
        let mut refresh = false;
        if refresh_run_counter == 0 {
            refresh = true;
        }
        if refresh {
            if let Ok(conn) = pool.get() {
                refresh_imds_items(&conn, &imds);
            } else {
                // TODO: log
            }
        }
        if refresh_run_counter >= 9 { refresh_run_counter = 0; } else { refresh_run_counter += 1; }
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
}

fn main() {
    let mut c = Config::new();

    c.merge(File::with_name("/etc/jaspy/poller.yml").required(false)).unwrap()
        .merge(File::with_name("~/.config/jaspy/poller.yml").required(false)).unwrap()
        .merge(Environment::with_prefix("JASPY")).unwrap();

    let running = Arc::new(AtomicBool::new(true));
    let msgbus : Arc<Mutex<utilities::msgbus::MessageBus>> = Arc::new(Mutex::new(utilities::msgbus::MessageBus::new()));
    let imds : Arc<Mutex<utilities::imds::IMDS>> = Arc::new(Mutex::new(utilities::imds::IMDS::new(msgbus.clone())));

    let imds_worker_imds = imds.clone();
    let imds_worker_running = running.clone();
    let imds_worker_thread = std::thread::spawn(|| {
        imds_worker(imds_worker_running, imds_worker_imds);
    });

    let cache_controller : Arc<Mutex<utilities::cache::CacheController>> = Arc::new(Mutex::new(utilities::cache::CacheController::new()));

    let runtime_info : Arc<Mutex<models::internal::RuntimeInfo>> = Arc::new(Mutex::new(models::internal::RuntimeInfo::new()));

    rocket::ignite()
        .attach(db::JaspyDB::fairing())
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
        .manage(cache_controller.clone())
        .manage(runtime_info.clone())
        .manage(msgbus.clone())
        .launch();

    (*running).store(false, std::sync::atomic::Ordering::Relaxed);
    imds_worker_thread.join().unwrap();
}
