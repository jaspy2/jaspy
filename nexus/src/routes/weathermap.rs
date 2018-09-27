extern crate rocket_contrib;
use models;
use db;
use rocket_contrib::Json;
use std::collections::HashMap;

#[get("/")]
fn full_topology_data(connection: db::Connection) -> Json<models::json::WeathermapBase> {
    let mut wmap: models::json::WeathermapBase = models::json::WeathermapBase {
        devices: HashMap::new(),
    };
    let devices = models::dbo::Device::monitored(&connection);
    for device in devices.iter() {
        let device_fqdn = format!("{}.{}", device.name, device.dns_domain);
        let mut weathermap_device = models::json::WeathermapDevice {
            fqdn: device_fqdn.clone(),
            interfaces: HashMap::new(),
        };

        for interface in device.interfaces(&connection) {
            let connected_interface: Option<models::json::WeathermapDeviceInterfaceConnectedTo> = match interface.peer_interface(&connection) {
                Some(peer_interface) => {
                    let peer_device = peer_interface.device(&connection);
                    let peer_device_fqdn = format!("{}.{}", peer_device.name, peer_device.dns_domain);
                    
                    Some(models::json::WeathermapDeviceInterfaceConnectedTo {
                        fqdn: peer_device_fqdn,
                        interface: peer_interface.name(),
                    })
                },
                None => {
                    None
                }
            };
            let mut weathermap_interface = models::json::WeathermapDeviceInterface {
                name: interface.name(),
                if_index: interface.index,
                connected_to: connected_interface
            };
            weathermap_device.interfaces.insert(
                interface.name(),
                weathermap_interface,
            );
        }
        
        wmap.devices.insert(device_fqdn.clone(), weathermap_device);
    }
    return Json(wmap);
}