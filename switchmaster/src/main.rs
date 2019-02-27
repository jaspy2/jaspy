#![feature(proc_macro_hygiene, decl_macro)]
#[macro_use] extern crate serde_derive;
extern crate rocket;
#[macro_use] extern crate rocket_codegen;
#[macro_use] extern crate diesel;
extern crate kerosiini;

mod models;
mod repositories;
mod schema;
mod routes;

fn main() {
    let connection_string = "postgres://jaspy:jaspy@localhost/switchmaster";

    rocket::ignite()
        .mount(
            "/switch",
            routes![
                routes::switches::list,
                routes::switches::find,
                routes::switches::create,
                routes::switches::update,
                routes::switches::delete
            ]
        )
        .manage(kerosiini::managed_pool::create_managed_pool(connection_string.to_string()))
        .launch();
}
