use crate::models;
use crate::db;
use crate::utilities;
use rocket::{get, put};
use rocket::serde::json::Json;
use std::sync::{Arc,Mutex};
use rocket::State;

// TODO: GH#9 Move everything to v1 API
#[get("/?<device_fqdn>")]
pub fn interface_list(mut connection: db::JaspyDB, device_fqdn: Option<String>) -> Json<Vec<models::dbo::Interface>> {
    match device_fqdn {
        Some(device_fqdn) => {
            if let Some(device) = models::dbo::Device::find_by_fqdn(&mut connection, &device_fqdn) {
                let interfaces = device.interfaces(&mut connection);
                return Json(interfaces);
            };
            return Json(Vec::new());
        },
        None => {
            return Json(models::dbo::Interface::all(&mut connection));
        }
    };
}

#[put("/monitor", data = "<interface_monitor_report>")]
pub fn interface_monitor_report(mut connection: db::JaspyDB, imds: &State<Arc<Mutex<utilities::imds::IMDS>>>, interface_monitor_report : Json<models::json::InterfaceMonitorReport>) {
    if let Ok(ref mut imds) = imds.lock() {
        imds.report_interfaces(&mut connection, interface_monitor_report.into_inner());
    }
}
