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

fn main() {
    let imds : Arc<Mutex<utilities::imds::IMDS>> = Arc::new(Mutex::new(utilities::imds::IMDS::new()));

    if let Ok(ref mut imds) = imds.lock() {
        imds.refresh_device(&"test.fqdn".to_string());
        imds.refresh_interface(&"test.fqdn".to_string(), 1234, &"interfacePort1".to_string(), false, None);
        imds.refresh_interface(&"test.fqdn".to_string(), 123, &"interfacePort2".to_string(), false, None);
    };
    
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
        .launch();
}
