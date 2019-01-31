extern crate rocket_contrib;
use models;
use db;
use rocket::{get, put};
use rocket_contrib::json;
use std::sync::{Arc,Mutex};
use rocket::State;
use utilities;

#[get("/")]
pub fn device_list(connection: db::Connection) -> json::Json<Vec<models::dbo::Device>> {
    return json::Json(models::dbo::Device::all(&connection));
}

#[put("/", data = "<device_json>")]
pub fn device_create_or_modify(connection: db::Connection, device_json: rocket_contrib::json::Json<models::dbo::NewDevice>, cache_controller: State<Arc<Mutex<utilities::cache::CacheController>>>, msgbus: State<Arc<Mutex<utilities::msgbus::MessageBus>>>) -> Option<json::Json<models::dbo::Device>> {
    let mut device : models::dbo::Device;
    if let Some(old_device) = models::dbo::Device::find_by_hostname_and_domain_name(&connection, &device_json.name, &device_json.dns_domain) {
        let device_fqdn = format!("{}.{}", old_device.name, old_device.dns_domain);
        let mut changed = false;
        device = old_device;
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
                // TODO: return 500
                return None;
            }
        }
    } else {
        if let Ok(created_device) = models::dbo::Device::create(&device_json, &connection) {
            let device_fqdn = format!("{}.{}", created_device.name, created_device.dns_domain);
            if let Ok(ref cache_controller) = cache_controller.lock() { cache_controller.invalidate_weathermap_cache(); }
            device = created_device;
            let event = models::events::Event::device_created_event(&device_fqdn);
            if let Ok(ref mut msgbus) = msgbus.lock() {
                msgbus.event(event);
            }
        } else {
            // TODO: return 500
            return None;
        }
    }
    return Some(json::Json(device));
}

#[delete("/", data = "<device_json>")]
pub fn device_delete(connection: db::Connection, device_json: rocket_contrib::json::Json<models::dbo::NewDevice>, cache_controller: State<Arc<Mutex<utilities::cache::CacheController>>>, msgbus: State<Arc<Mutex<utilities::msgbus::MessageBus>>>) -> Option<json::Json<models::dbo::Device>> {
    if let Some(old_device) = models::dbo::Device::find_by_hostname_and_domain_name(&connection, &device_json.name, &device_json.dns_domain) {
        if let Err(d) = old_device.delete(&connection) {
            println!("{}", d);
            // TODO: return 500
            return None;
        } else {
            let device_fqdn = format!("{}.{}", old_device.name, old_device.dns_domain);
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
