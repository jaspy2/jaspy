extern crate rocket_contrib;
use crate::models;
use crate::db;
use rocket_contrib::json;

// TODO: GH#9 Move everything to v1 API
#[get("/?<client_address>")]
pub fn get_clientlocation(connection: db::Connection, client_address: String) -> Option<json::Json<models::dbo::ClientLocation>> {
    if let Some(existing_client_info) = models::dbo::ClientLocation::by_ip(&client_address, &connection) {
        return Some(json::Json(existing_client_info));
    } else {
        return None;
    }
}

#[put("/", data = "<client_location_info_json>")]
pub fn put_clientlocation(connection: db::Connection, client_location_info_json: json::Json<models::json::ClientLocationInfo>) {
    let client_location_info: models::json::ClientLocationInfo = client_location_info_json.into_inner();
    let option82_001: String;
    let option82_002: String;
    if let Some(opt82_001) = client_location_info.option82.get(&"001".to_string()) {
        option82_001 = opt82_001.clone();
    } else {
        return;
    }
    if let Some(opt82_002) = client_location_info.option82.get(&"002".to_string()) {
        option82_002 = opt82_002.clone();
    } else {
        return;
    }

    let option82_001_hex: Vec<&str> = option82_001.split(":").collect();
    let option82_002_hex: Vec<&str> = option82_002.split(":").collect();

    if option82_001_hex.len() == 6 && option82_002_hex.len() == 8 {
        let module: i32;
        if let Ok(module_res) = i32::from_str_radix(option82_001_hex[4], 16) {
            module = module_res;
        } else {
            return;
        }
        let port: i32;
        if let Ok(port_res) = i32::from_str_radix(option82_001_hex[5], 16) {
            port = port_res;
        } else {
            return;
        }
        let port_info = format!("{}/{}", module, port);
        let switch_base_mac = format!(
            "{}:{}:{}:{}:{}:{}",
            option82_002_hex[2], option82_002_hex[3], option82_002_hex[4],
            option82_002_hex[5], option82_002_hex[6], option82_002_hex[7]
        );
        let device;
        if let Some(some_device) = models::dbo::Device::by_base_mac(&switch_base_mac, &connection) {
            device = some_device;
        } else {
            return;
        }
        if let Some(mut existing_client_info) = models::dbo::ClientLocation::by_ip(&client_location_info.yiaddr, &connection) {
            if existing_client_info.port_info != port_info || existing_client_info.device_id != device.id {
                existing_client_info.port_info = port_info;
                existing_client_info.hw_address = client_location_info.chaddr.clone();
                existing_client_info.device_id = device.id;
                if let Err(_update_error) = existing_client_info.update(&connection) {
                    // TODO: log
                }
            }
        } else {
            let l = models::dbo::NewClientLocation {
                device_id: device.id,
                ip_address: client_location_info.yiaddr.clone(),
                hw_address: client_location_info.chaddr.clone(),
                port_info: port_info.clone()
            };
            if let Ok(_new_client_location) = models::dbo::ClientLocation::create(&l, &connection) {
            } else {
                // TODO: log this
            }
        }
    }
}
