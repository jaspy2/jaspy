extern crate rocket_contrib;
use models;
use db;
use rocket::{get, put};
use rocket_contrib::json;
use std::sync::{Arc,Mutex};
use rocket::State;
use utilities;

#[get("/device/<device_fqdn>/status")]
pub fn device_status(device_fqdn: String, imds: State<Arc<Mutex<utilities::imds::IMDS>>>) -> Option<json::Json<models::json::DeviceStatus>> {
    if let Ok(ref mut imds) = imds.inner().lock() {
        if let Some(device_metric) = imds.get_device(&device_fqdn) {
            let ret = models::json::DeviceStatus {
                fqdn: device_metric.fqdn.clone(),
                up: device_metric.up,
            };
            return Some(json::Json(ret));
        }
    }
    return None;
}

#[get("/device/<device_fqdn>/interfaces")]
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

#[get("/device/<device_fqdn>/status/interfaces")]
pub fn device_interface_status(device_fqdn: String, imds: State<Arc<Mutex<utilities::imds::IMDS>>>) -> Option<json::Json<Vec<models::json::DeviceInterfaceStatus>>> {
    if let Ok(ref mut imds) = imds.inner().lock() {
        if let Some(device_metric) = imds.get_device(&device_fqdn) {
            let mut ret_ifaces: Vec<models::json::DeviceInterfaceStatus> = Vec::new();
            for (_idx, interface_metric) in device_metric.interfaces.iter() {
                let reported_speed = match interface_metric.speed_override {
                    Some(speed_override) => Some(speed_override),
                    None => interface_metric.speed
                };
                ret_ifaces.push(models::json::DeviceInterfaceStatus {
                    name: interface_metric.name.clone(),
                    neighbors: interface_metric.neighbors,
                    up: interface_metric.up,
                    speed: reported_speed,
                    interface_type: interface_metric.interface_type.clone(),
                });
            }
            return Some(json::Json(ret_ifaces));
        }
    }
    return None;
}
