extern crate rocket_contrib;
use models;
use db;
use rocket_contrib::Json;
use std::sync::{Arc,Mutex};
use rocket::State;
use utilities;

#[get("/")]
fn device_list(connection: db::Connection) -> Json<Vec<models::dbo::Device>> {
    return Json(models::dbo::Device::all(&connection));
}

#[get("/monitor")]
fn monitored_device_list(connection: db::Connection, imds: State<Arc<Mutex<utilities::imds::IMDS>>>) -> Json<models::json::DeviceMonitorResponse> {
    let mut dmi : Vec<models::json::DeviceMonitorInfo> = Vec::new();
    for monitored in models::dbo::Device::monitored(&connection).iter() {
        if let Ok(ref mut imds) = imds.lock() {
            let device_fqdn = format!("{}.{}", monitored.name, monitored.dns_domain);
            if let Some(imds_device) = imds.get_device(&device_fqdn) {
                dmi.push(models::json::DeviceMonitorInfo { fqdn: device_fqdn, up: imds_device.up });
            }
        }
    }
    return Json(models::json::DeviceMonitorResponse { devices : dmi });
}

#[put("/monitor", data = "<device_monitor_report>")]
fn monitored_device_report(imds: State<Arc<Mutex<utilities::imds::IMDS>>>, device_monitor_report : Json<models::json::DeviceMonitorReport>) {
    if let Ok(ref mut imds) = imds.lock() {
        imds.report_device(device_monitor_report.into_inner());
    }
}