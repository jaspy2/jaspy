#![feature(custom_attribute)]
#![feature(plugin)]
#![plugin(rocket_codegen)]
#![allow(proc_macro_derive_resolution_fallback)] // remove when able
#[macro_use] extern crate serde_derive;
#[macro_use] extern crate diesel;
extern crate rocket;
extern crate rocket_contrib;
extern crate r2d2;
extern crate r2d2_diesel;
mod routes;
mod models;
mod db;
mod schema;
mod utilities;
use std::sync::{Arc, Mutex};
use std::sync::atomic::AtomicBool;

fn should_continue(running : &std::sync::atomic::AtomicBool) -> bool {
    return running.load(std::sync::atomic::Ordering::Relaxed);
}

fn imds_worker(running : Arc<AtomicBool>, imds : Arc<Mutex<utilities::imds::IMDS>>, metric_miss_cache: Arc<Mutex<models::metrics::DeviceMetricRefreshCacheMiss>>) {
    let mut initial_run : bool = true;
    loop {
        if !should_continue(&running) { break; }
        // get devices & ports
        // for port in ports
        // if initial_run then add to list and get ifinfo
        // if not initial run and metric_miss_cache.hasvalue(device_fqdn) then add to list and get ifinfo
        {
            if let Ok(ref mut metric_miss_cache) = metric_miss_cache.lock() {
                metric_miss_cache.miss_set.clear();
            }
        }
        {
            // refresh results :)
            if let Ok(ref mut imds) = imds.lock() {
                imds.refresh_device(&"test.fqdn".to_string());
                imds.refresh_interface(&"test.fqdn".to_string(), 1234, &"interfacePort1".to_string(), false, None);
                imds.refresh_interface(&"test.fqdn".to_string(), 123, &"interfacePort2".to_string(), false, None);
            };
        }
        initial_run = false;
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
}

fn main() {
    let running = Arc::new(AtomicBool::new(true));
    let imds : Arc<Mutex<utilities::imds::IMDS>> = Arc::new(Mutex::new(utilities::imds::IMDS::new()));
    let metric_miss_cache : Arc<Mutex<models::metrics::DeviceMetricRefreshCacheMiss>> = Arc::new(Mutex::new(models::metrics::DeviceMetricRefreshCacheMiss::new()));

    let imds_worker_imds = imds.clone();
    let imds_worker_running = running.clone();
    let imds_worder_metric_miss_cache = metric_miss_cache.clone();
    let imds_worker_thread = std::thread::spawn(|| {
        imds_worker(imds_worker_running, imds_worker_imds, imds_worder_metric_miss_cache);
    });
    
    rocket::ignite()
        .mount(
            "/discovery",
            routes![
                routes::discovery::discovery_device,
                routes::discovery::discovery_links
            ]
        )
        .mount(
            "/device",
            routes![
                routes::device::device_list,
                routes::device::monitored_device_list,
            ]
        )
        .mount(
            "/metrics",
            routes![
                routes::metrics::metrics_fast,
            ]
        )
        .manage(db::connect())
        .manage(imds.clone())
        .manage(metric_miss_cache.clone())
        .launch();

    (*running).store(false, std::sync::atomic::Ordering::Relaxed);
    imds_worker_thread.join().unwrap();
}
