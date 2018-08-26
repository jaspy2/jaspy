extern crate rocket_contrib;
use models;
use db;
use rocket_contrib::Json;
use std::sync::{Arc,Mutex};
use rocket::State;
use utilities;

#[get("/")]
fn interface_list(connection: db::Connection) -> Json<Vec<models::dbo::Interface>> {
    return Json(models::dbo::Interface::all(&connection));
}

#[put("/monitor", data = "<interface_monitor_report>")]
fn interface_monitor_report(imds: State<Arc<Mutex<utilities::imds::IMDS>>>, interface_monitor_report : Json<models::json::InterfaceMonitorReport>) {
    if let Ok(ref mut imds) = imds.lock() {
        imds.report_interfaces(interface_monitor_report.into_inner());
    }
}