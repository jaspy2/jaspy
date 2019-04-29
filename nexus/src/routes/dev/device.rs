extern crate rocket_contrib;
use models;
use db;
use rocket::{get, put};
use rocket_contrib::json;
use std::sync::{Arc,Mutex};
use rocket::State;
use utilities;

#[get("/")]
pub fn list(connection: db::Connection) -> json::Json<Vec<models::dbo::Device>> {
    return json::Json(models::dbo::Device::all(&connection));
}

#[get("/<device_fqdn>")]
pub fn get_device(connection: db::Connection, device_fqdn: String) -> Option<json::Json<models::dbo::Device>> {
    if let Some(device) = models::dbo::Device::find_by_fqdn(&connection, &device_fqdn) {
        return Some(json::Json(device));
    } else {
        return None;
    }
}

#[post("/", data = "<device_json>")]
pub fn create(device_json: rocket_contrib::json::Json<models::dbo::NewDevice>, connection: db::Connection, cache_controller: State<Arc<Mutex<utilities::cache::CacheController>>>, msgbus: State<Arc<Mutex<utilities::msgbus::MessageBus>>>) -> Option<json::Json<models::dbo::Device>> {
    if let Ok(created_device) = models::dbo::Device::create(&device_json, &connection) {
        let device_fqdn = format!("{}.{}", created_device.name, created_device.dns_domain);
        if let Ok(ref cache_controller) = cache_controller.lock() { cache_controller.invalidate_weathermap_cache(); }
        let event = models::events::Event::device_created_event(&device_fqdn);
        if let Ok(ref mut msgbus) = msgbus.lock() {
            msgbus.event(event);
        }
        return Some(json::Json(created_device));
    }
    // TODO: Return 400 or 500, need details from creation failure
    return None;
}

#[put("/<device_fqdn>", data = "<device_json>")]
pub fn update(device_fqdn: String, device_json: rocket_contrib::json::Json<models::dbo::NewDevice>, connection: db::Connection, cache_controller: State<Arc<Mutex<utilities::cache::CacheController>>>, msgbus: State<Arc<Mutex<utilities::msgbus::MessageBus>>>) -> Option<json::Json<models::dbo::Device>> {
    if let Some(mut device) = models::dbo::Device::find_by_fqdn(&connection, &device_fqdn) {
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
            // TODO: this MUST NOT raise an event!
            changed = true;
            device.snmp_community = device_json.snmp_community.clone();
        }
        if changed {
            if let Err(_) = device.update(&connection) {
                // TODO: return 500 or 400
                return None;
            }
        }
        return Some(json::Json(device));
    }
    // TODO: Return 404
    return None;
}

#[delete("/<device_fqdn>")]
pub fn delete(connection: db::Connection, device_fqdn: String, cache_controller: State<Arc<Mutex<utilities::cache::CacheController>>>, msgbus: State<Arc<Mutex<utilities::msgbus::MessageBus>>>) -> Option<json::Json<models::dbo::Device>> {
    if let Some(old_device) = models::dbo::Device::find_by_fqdn(&connection, &device_fqdn) {
        if let Err(d) = old_device.delete(&connection) {
            println!("{}", d);
            // TODO: return 500
            return None;
        } else {
            if let Ok(ref cache_controller) = cache_controller.lock() { cache_controller.invalidate_weathermap_cache(); }
            let event = models::events::Event::device_deleted_event(&device_fqdn);
            if let Ok(ref mut msgbus) = msgbus.lock() {
                msgbus.event(event);
            }
            return Some(json::Json(old_device));
        }
    } else {
        return None;
    }
}

#[get("/<device_fqdn>/interfaces")]
pub fn interfaces(connection: db::Connection, device_fqdn: Option<String>) -> json::Json<Vec<models::dbo::Interface>> {
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

#[get("/monitor")]
pub fn monitored_device_list(connection: db::Connection, imds: State<Arc<Mutex<utilities::imds::IMDS>>>, runtime_info: State<Arc<Mutex<models::internal::RuntimeInfo>>>) -> json::Json<models::json::DeviceMonitorResponse> {
    let mut dmi : Vec<models::json::DeviceMonitorInfo> = Vec::new();
    for monitored in models::dbo::Device::monitored(&connection).iter() {
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
    return json::Json(models::json::DeviceMonitorResponse { state_id: state_id, devices: dmi });
}

#[put("/monitor", data = "<device_monitor_report>")]
pub fn monitored_device_report(connection: db::Connection, imds: State<Arc<Mutex<utilities::imds::IMDS>>>, device_monitor_report : json::Json<models::json::DeviceMonitorReport>) {
    if let Ok(ref mut imds) = imds.lock() {
        imds.report_device(&connection, device_monitor_report.into_inner());
    }
}

#[get("/<device_fqdn>/status")]
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

#[get("/<device_fqdn>/status/interfaces")]
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
