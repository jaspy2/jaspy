extern crate rocket_contrib;
use models;
use db;
use rocket::{get, put};
use rocket_contrib::json;
use std::sync::{Arc,Mutex};
use rocket::State;
use utilities;

#[get("/")]
pub fn interface_list(connection: db::Connection) -> json::Json<Vec<models::dbo::Interface>> {
    return json::Json(models::dbo::Interface::all(&connection));
}

#[put("/monitor", data = "<interface_monitor_report>")]
pub fn interface_monitor_report(connection: db::Connection, imds: State<Arc<Mutex<utilities::imds::IMDS>>>, interface_monitor_report : json::Json<models::json::InterfaceMonitorReport>) {
    if let Ok(ref mut imds) = imds.lock() {
        imds.report_interfaces(&connection, interface_monitor_report.into_inner());
    }
}
