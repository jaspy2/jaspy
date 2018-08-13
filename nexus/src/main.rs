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

fn main() {
    rocket::ignite()
        .mount(
            "/discovery",
            routes![
                routes::discovery::discovery_device,
                routes::discovery::discovery_links
            ]
        )
        .manage(db::connect())
        .launch();
}
