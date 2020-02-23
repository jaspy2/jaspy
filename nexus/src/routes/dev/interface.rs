extern crate rocket_contrib;
use crate::models;
use crate::db;
use crate::utilities;
use rocket::{get, put};
use rocket_contrib::json;
use std::sync::{Arc,Mutex};
use rocket::State;

// TODO: GH#9 Move everything to v1 API
#[get("/?<device_fqdn>")]
pub fn interface_list(connection: db::Connection, device_fqdn: Option<String>) -> json::Json<Vec<models::dbo::Interface>> {
    match device_fqdn {
        Some(device_fqdn) => {
            if let Some(device) = models::dbo::Device::find_by_fqdn(&connection, &device_fqdn) {
                let interfaces = device.interfaces(&connection);
                return json::Json(interfaces);
            };
            return json::Json(Vec::new());
        },
        None => {
            return json::Json(models::dbo::Interface::all(&connection));
        }
    };
}

#[put("/monitor", data = "<interface_monitor_report>")]
pub fn interface_monitor_report(connection: db::Connection, imds: State<Arc<Mutex<utilities::imds::IMDS>>>, interface_monitor_report : json::Json<models::json::InterfaceMonitorReport>) {
    if let Ok(ref mut imds) = imds.lock() {
        imds.report_interfaces(&connection, interface_monitor_report.into_inner());
    }
}

