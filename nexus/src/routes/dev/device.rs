use crate::models;
use crate::db;
use crate::utilities;
use rocket::{get, post, put, delete};
use rocket::serde::json::Json;
use std::sync::{Arc,Mutex};
use rocket::State;

#[get("/")]
pub fn list(mut connection: db::JaspyDB) -> Json<Vec<models::dbo::Device>> {
    return Json(models::dbo::Device::all(&mut connection));
}

#[get("/<device_fqdn>")]
pub fn get_device(mut connection: db::JaspyDB, device_fqdn: &str) -> Option<Json<models::dbo::Device>> {
    if let Some(device) = models::dbo::Device::find_by_fqdn(&mut connection, &device_fqdn) {
        return Some(Json(device));
    } else {
        return None;
    }
}

#[post("/", data = "<device_json>")]
pub fn create(device_json: Json<models::dbo::NewDevice>, mut connection: db::JaspyDB, cache_controller: &State<Arc<Mutex<utilities::cache::CacheController>>>, msgbus: &State<Arc<Mutex<utilities::msgbus::MessageBus>>>) -> Option<Json<models::dbo::Device>> {
    if let Ok(created_device) = models::dbo::Device::create(&device_json, &mut connection) {
        let device_fqdn = format!("{}.{}", created_device.name, created_device.dns_domain);
        if let Ok(ref cache_controller) = cache_controller.lock() { cache_controller.invalidate_weathermap_cache(); }
        let event = models::events::Event::device_created_event(&device_fqdn);
        if let Ok(ref mut msgbus) = msgbus.lock() {
            msgbus.event(event);
        }
        return Some(Json(created_device));
    }
    // TODO: Return 400 or 500, need details from creation failure
    return None;
}

#[put("/<device_fqdn>", data = "<device_json>")]
pub fn update(device_fqdn: &str, device_json: Json<models::dbo::NewDevice>, mut connection: db::JaspyDB, msgbus: &State<Arc<Mutex<utilities::msgbus::MessageBus>>>) -> Option<Json<models::dbo::Device>> {
    if let Some(mut device) = models::dbo::Device::find_by_fqdn(&mut connection, &device_fqdn) {
        if format!("{}.{}", device.name, device.dns_domain) != device_fqdn {
            // TODO: return 400
            return None;
        }

        let mut changed = false;
        if device.polling_enabled != device_json.polling_enabled {
            let event = models::events::Event::device_polling_changed_event(
                &device_fqdn, device.polling_enabled, device_json.polling_enabled);
            if let Ok(ref mut msgbus) = msgbus.lock() {
                msgbus.event(event);
            }
            changed = true;
            device.polling_enabled = device_json.polling_enabled.clone();
        }
        if device.os_info != device_json.os_info {
            let event = models::events::Event::device_os_info_changed_event(
                &device_fqdn, &device.os_info, &device_json.os_info);
            if let Ok(ref mut msgbus) = msgbus.lock() {
                msgbus.event(event);
            }
            changed = true;
            device.os_info = device_json.os_info.clone();
        }
        if device.base_mac != device_json.base_mac {
            let event = models::events::Event::device_base_mac_changed_event(
                &device_fqdn, &device.base_mac, &device_json.base_mac);
            if let Ok(ref mut msgbus) = msgbus.lock() {
                msgbus.event(event);
            }
            changed = true;
            device.base_mac = device_json.base_mac.clone();
        }
        if device.snmp_community != device_json.snmp_community {
            // This MUST NOT raise an event!
            changed = true;
            device.snmp_community = device_json.snmp_community.clone();
        }
        if changed {
            if let Err(_) = device.update(&mut connection) {
                // TODO: return 500 or 400
                return None;
            }
        }
        return Some(Json(device));
    }
    // TODO: Return 404
    return None;
}

#[delete("/<device_fqdn>")]
pub fn delete(mut connection: db::JaspyDB, device_fqdn: &str, cache_controller: &State<Arc<Mutex<utilities::cache::CacheController>>>, msgbus: &State<Arc<Mutex<utilities::msgbus::MessageBus>>>) -> Option<Json<models::dbo::Device>> {
    if let Some(old_device) = models::dbo::Device::find_by_fqdn(&mut connection, &device_fqdn) {
        if let Err(d) = old_device.delete(&mut connection) {
            println!("{}", d);
            // TODO: return 500
            return None;
        } else {
            if let Ok(ref cache_controller) = cache_controller.lock() { cache_controller.invalidate_weathermap_cache(); }
            let event = models::events::Event::device_deleted_event(&device_fqdn);
            if let Ok(ref mut msgbus) = msgbus.lock() {
                msgbus.event(event);
            }
            return Some(Json(old_device));
        }
    } else {
        return None;
    }
}

#[get("/<device_fqdn>/interfaces")]
pub fn interfaces(mut connection: db::JaspyDB, device_fqdn: Option<String>) -> Json<Vec<models::dbo::Interface>> {
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

#[get("/monitor")]
pub fn monitored_device_list(mut connection: db::JaspyDB, imds: &State<Arc<Mutex<utilities::imds::IMDS>>>, runtime_info: &State<Arc<Mutex<models::internal::RuntimeInfo>>>) -> Json<models::json::DeviceMonitorResponse> {
    let mut dmi : Vec<models::json::DeviceMonitorInfo> = Vec::new();
    for monitored in models::dbo::Device::monitored(&mut connection).iter() {
        if let Ok(ref mut imds) = imds.lock() {
            let device_fqdn = format!("{}.{}", monitored.name, monitored.dns_domain);
            if let Some(imds_device) = imds.get_device(&device_fqdn) {
                dmi.push(models::json::DeviceMonitorInfo { fqdn: device_fqdn, up: imds_device.up });
            }
        }
    }
    let state_id : i64;
    if let Ok(ref rti) = runtime_info.lock() {
        state_id = rti.state_id();
    } else {
        state_id = 0;
    }
    return Json(models::json::DeviceMonitorResponse { state_id: state_id, devices: dmi });
}

#[put("/monitor", data = "<device_monitor_report>")]
pub fn monitored_device_report(mut connection: db::JaspyDB, imds: &State<Arc<Mutex<utilities::imds::IMDS>>>, device_monitor_report : Json<models::json::DeviceMonitorReport>) {
    if let Ok(ref mut imds) = imds.lock() {
        imds.report_device(&mut connection, device_monitor_report.into_inner());
    }
}

#[get("/<device_fqdn>/status")]
pub fn device_status(device_fqdn: &str, imds: &State<Arc<Mutex<utilities::imds::IMDS>>>) -> Option<Json<models::json::DeviceStatus>> {
    if let Ok(ref mut imds) = imds.inner().lock() {
        if let Some(device_metric) = imds.get_device(&device_fqdn) {
            let ret = models::json::DeviceStatus {
                fqdn: device_metric.fqdn.clone(),
                up: device_metric.up,
            };
            return Some(Json(ret));
        }
    }
    return None;
}

#[get("/<device_fqdn>/status/interfaces")]
pub fn device_interface_status(device_fqdn: &str, imds: &State<Arc<Mutex<utilities::imds::IMDS>>>) -> Option<Json<Vec<models::json::DeviceInterfaceStatus>>> {
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
            return Some(Json(ret_ifaces));
        }
    }
    return None;
}

#[delete("/connections?<device_fqdn>")]
pub fn clear_device_connection(mut connection: db::JaspyDB, device_fqdn: &str) {
    if let Some(device) = models::dbo::Device::find_by_fqdn(&mut connection, &device_fqdn) {
        let interfaces = device.interfaces(&mut connection);
        for mut interface in interfaces {
            if let Some(mut peer_interface) = interface.peer_interface(&mut connection) {
                peer_interface.connected_interface = None;
                if let Err(_) = peer_interface.update(&mut connection) {
                    // TODO: log
                }
                interface.connected_interface = None;
                if let Err(_) = interface.update(&mut connection) {
                    // TODO: log
                }
            }
        }
    };
}
