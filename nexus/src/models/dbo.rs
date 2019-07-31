use schema::{devices,interfaces,weathermap_device_infos,client_locations};
use diesel;
use diesel::pg::PgConnection;
use diesel::BelongingToDsl;
use diesel::RunQueryDsl;
use diesel::QueryDsl;
use diesel::ExpressionMethods;
use diesel::BoolExpressionMethods;

#[table_name = "devices"]
#[derive(Insertable, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewDevice {
    pub name: String,
    pub dns_domain: String,
    pub snmp_community: Option<String>,
    pub base_mac: Option<String>,
    pub polling_enabled: Option<bool>,
    pub os_info: Option<String>,
    pub device_type: Option<String>,
    pub software_version: Option<String>,
}

#[table_name = "interfaces"]
#[derive(Insertable)]
pub struct NewInterface {
    pub index: i32,
    pub interface_type: String,
    pub device_id: i32,
    pub name: String,
    pub alias: Option<String>,
    pub description: Option<String>,
}

#[table_name = "devices"]
#[derive(Serialize, Deserialize, Queryable, Identifiable, AsChangeset, Clone)]
#[serde(rename_all = "camelCase")]
#[changeset_options(treat_none_as_null = "true")]
pub struct Device {
    pub id: i32,
    pub name: String,
    pub dns_domain: String,
    pub snmp_community: Option<String>,
    pub base_mac: Option<String>,
    pub polling_enabled: Option<bool>,
    pub os_info: Option<String>,
    pub device_type: Option<String>,
    pub software_version: Option<String>,
}

pub struct UpdatedWeathermapDeviceInfo {
    pub x: f64,
    pub y: f64,
    pub super_node: bool,
    pub expanded_by_default: bool,
}

#[table_name = "weathermap_device_infos"]
#[derive(Insertable)]
pub struct NewWeathermapDeviceInfo {
    pub x: f64,
    pub y: f64,
    pub super_node: bool,
    pub expanded_by_default: bool,
    pub device_id: i32,
}

#[table_name = "client_locations"]
#[derive(Insertable)]
pub struct NewClientLocation {
    pub device_id: i32,
    pub ip_address: String,
    pub port_info: String,
}

#[belongs_to(Device)]
#[table_name = "client_locations"]
#[derive(Serialize, Deserialize, Queryable, Identifiable, AsChangeset, Associations, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ClientLocation {
    pub id: i32,
    pub device_id: i32,
    pub ip_address: String,
    pub port_info: String,
}

#[belongs_to(Device)]
#[table_name = "weathermap_device_infos"]
#[derive(Serialize, Deserialize, Queryable, Identifiable, AsChangeset, Associations, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WeathermapDeviceInfo {
    pub id: i32,
    pub x: f64,
    pub y: f64,
    pub super_node: bool,
    pub expanded_by_default: bool,
    pub device_id: i32,
}

#[belongs_to(Device)]
#[table_name = "interfaces"]
#[derive(Serialize, Deserialize, Queryable, Identifiable, AsChangeset, Associations, Clone)]
#[serde(rename_all = "camelCase")]
#[changeset_options(treat_none_as_null = "true")]
pub struct Interface {
    pub id: i32,
    pub index: i32,
    pub interface_type: String,
    pub connected_interface: Option<i32>,
    pub device_id: i32,
    pub display_name: Option<String>,
    pub name: String,
    pub alias: Option<String>,
    pub description: Option<String>,
    pub polling_enabled: Option<bool>,
    pub speed_override: Option<i32>,
    pub virtual_connection: Option<i32>,
}

impl ClientLocation {
    pub fn create(new_client_location: &NewClientLocation, connection: &PgConnection) -> Result<ClientLocation, diesel::result::Error> {
        let result = diesel::insert_into(client_locations::table)
            .values(new_client_location)
            .get_result(connection);
        return result;
    }

    pub fn update(self: &ClientLocation, connection: &PgConnection) -> Result<usize, diesel::result::Error> {
        return diesel::update(client_locations::table.find(self.id)).set(self).execute(connection);
    }
    pub fn by_ip(ip_address: &String, connection: &PgConnection) -> Option<ClientLocation> {
        match client_locations::table
            .filter(client_locations::ip_address.eq(ip_address))
            .first::<ClientLocation>(connection)    
        {
            Ok(client_location) => {
                return Some(client_location);
            },
            Err(_) => {
                return None;
            }
        }
    }
}

impl Device {
    pub fn by_id(id: i32, connection: &PgConnection) -> Option<Device> {
        match devices::table
            .filter(devices::id.eq(id))
            .first::<Device>(connection)    
        {
            Ok(device) => {
                return Some(device);
            },
            Err(_) => {
                return None;
            }
        }
    }

    pub fn by_base_mac(base_mac: &String, connection: &PgConnection) -> Option<Device> {
        match devices::table
            .filter(devices::base_mac.eq(base_mac))
            .first::<Device>(connection)    
        {
            Ok(device) => {
                return Some(device);
            },
            Err(_) => {
                return None;
            }
        }
    }

    pub fn all(connection: &PgConnection) -> Vec<Device> {
        match devices::table.load(connection) {
            Ok(result) => {
                return result;
            },
            Err(_) => {
                return Vec::new();
            }
        }
    }

    pub fn monitored(connection: &PgConnection) -> Vec<Device> {
        // TODO: default setting for polling enabled? NULL might mean false in that case..
        match devices::table
            .filter(
                devices::polling_enabled.is_null()
                .or(devices::polling_enabled.eq(true))
            )
            .load(connection)
        {
            Ok(result) => {
                return result;
            },
            Err(_) => {
                return Vec::new();
            }
        }
    }

    pub fn interface_by_name(self: &Device, connection: &PgConnection, name: &String) -> Option<Interface> {
        match interfaces::table
            .filter(interfaces::device_id.eq(self.id))
            .filter(interfaces::name.eq(name))
            .first::<Interface>(connection)
        {
            Ok(interface) => {
                return Some(interface);
            },
            Err(_) => {
                return None;
            }
        }
    }

    pub fn interface_by_index(self: &Device, connection: &PgConnection, index: &i32) -> Option<Interface> {
        match interfaces::table
            .filter(interfaces::device_id.eq(self.id))
            .filter(interfaces::index.eq(index))
            .first::<Interface>(connection)
        {
            Ok(interface) => {
                return Some(interface);
            },
            Err(_) => {
                return None;
            }
        }
    }

    pub fn create(new_device: &NewDevice, connection: &PgConnection) -> Result<Device, diesel::result::Error> {
        let result = diesel::insert_into(devices::table)
            .values(new_device)
            .get_result(connection);
        return result;
    }

    pub fn find_by_hostname_and_domain_name(connection: &PgConnection, hostname: &String, domain_name: &String) -> Option<Device> {
        match devices::table
            .filter(devices::name.eq(hostname))
            .filter(devices::dns_domain.eq(domain_name))
            .first::<Device>(connection)
        {
            Ok(device) => {
                return Some(device);
            },
            Err(_) => {
                return None;
            }
        }
    }

    pub fn find_by_fqdn(connection: &PgConnection, fqdn: &String) -> Option<Device> {
        let fqdn_splitted : Vec<&str> = fqdn.splitn(2, ".").collect();
        if fqdn_splitted.len() != 2 {
            return None;
        }
        match devices::table
            .filter(devices::name.eq(fqdn_splitted[0]))
            .filter(devices::dns_domain.eq(fqdn_splitted[1]))
            .first::<Device>(connection)
        {
            Ok(device) => {
                return Some(device);
            },
            Err(_) => {
                return None;
            }
        }
    }

    pub fn update(self: &Device, connection: &PgConnection) -> Result<usize, diesel::result::Error> {
        return diesel::update(devices::table.find(self.id)).set(self).execute(connection);
    }

    pub fn delete(self: &Device, connection: &PgConnection) -> Result<usize, diesel::result::Error> {
        if let Ok(weathermap_device_info) = WeathermapDeviceInfo::belonging_to(self).load(connection) {
            let wmdis : Vec<WeathermapDeviceInfo> = weathermap_device_info;
            for wmdi in wmdis.iter() {
                if let Err(_) = wmdi.delete(connection) {
                    // TODO: log
                }
            }
            for interface in self.interfaces(connection).iter() {
                if let Err(d) = interface.delete(connection) {
                    // TODO: log
                    println!("{}", d);
                }
            }
        }

        return diesel::delete(devices::table.find(self.id)).execute(connection);
    }

    pub fn interfaces(self: &Device, connection: &PgConnection) -> Vec<Interface> {
        match Interface::belonging_to(self).load(connection) {
            Ok(result) => {
                return result;
            },
            Err(_) => {
                return Vec::new();
            }
        }
    }

    pub fn weathermap_info(self: &Device, connection: &PgConnection) -> Option<WeathermapDeviceInfo> {
        match weathermap_device_infos::table
            .filter(weathermap_device_infos::device_id.eq(self.id))
            .first::<WeathermapDeviceInfo>(connection)
        {
            Ok(weathermap_device_info) => {
                return Some(weathermap_device_info);
            },
            Err(_) => {
                return None;
            }
        }
    }
}

impl Interface {
    pub fn by_id(id: i32, connection: &PgConnection) -> Option<Interface> {
        match interfaces::table
            .filter(interfaces::id.eq(id))
            .first::<Interface>(connection)    
        {
            Ok(interface) => {
                return Some(interface);
            },
            Err(_) => {
                return None;
            }
        }
    }

    pub fn all(connection: &PgConnection) -> Vec<Interface> {
        match interfaces::table.load(connection) {
            Ok(result) => {
                return result;
            },
            Err(_) => {
                return Vec::new();
            }
        }
    }

    pub fn create(new_interface: &NewInterface, connection: &PgConnection) -> Result<Interface, diesel::result::Error> {
        let result = diesel::insert_into(interfaces::table)
            .values(new_interface)
            .get_result(connection);
        return result;
    }

    pub fn name(self: &Interface) -> String {
        if let Some(ref display_name) = self.display_name {
            return display_name.clone();
        } else {
            return self.name.clone();
        }
    }

    pub fn update(self: &Interface, connection: &PgConnection) -> Result<usize, diesel::result::Error> {
        return diesel::update(interfaces::table.find(self.id)).set(self).execute(connection);
    }

    pub fn delete(self: &Interface, connection: &PgConnection) -> Result<usize, diesel::result::Error> {
        match interfaces::table
            .filter(
                interfaces::connected_interface.eq(self.id)
                .or(interfaces::virtual_connection.eq(self.id))
            )
            .load::<Interface>(connection)
        {
            Ok(mut peer_interface_vec) => {
                for peer_interface in peer_interface_vec.iter_mut() {
                    peer_interface.connected_interface = None;
                    if let Err(_) = peer_interface.update(connection) {
                        // TODO: log
                    }
                }
            },
            Err(_) => {}
        }
        return diesel::delete(interfaces::table.find(self.id)).execute(connection);
    }

    pub fn peer_interface(self: &Interface, connection: &PgConnection) -> Option<Interface> {
        if let Some(connected_interface_id) = self.virtual_connection {
            match Interface::by_id(connected_interface_id, connection) {
                Some(peer_interface) => {
                    return Some(peer_interface);
                },
                None => {
                    // TODO: WTF, this cant happen, probably?
                    return None;
                }
            }
        } else if let Some(connected_interface_id) = self.connected_interface {
            match Interface::by_id(connected_interface_id, connection) {
                Some(peer_interface) => {
                    return Some(peer_interface);
                },
                None => {
                    // TODO: WTF, this cant happen, probably?
                    return None;
                }
            }
        } else {
            return None;
        }
    }

    pub fn device(self: &Interface, connection: &PgConnection) -> Device {
        return Device::by_id(self.device_id, connection).unwrap();
    }
}

impl WeathermapDeviceInfo {
    pub fn create(new_weathermap_device_info: &NewWeathermapDeviceInfo, connection: &PgConnection) -> Result<WeathermapDeviceInfo, diesel::result::Error> {
        let result = diesel::insert_into(weathermap_device_infos::table)
            .values(new_weathermap_device_info)
            .get_result(connection);
        return result;
    }

    pub fn update(self: &WeathermapDeviceInfo, connection: &PgConnection) -> Result<usize, diesel::result::Error> {
        return diesel::update(weathermap_device_infos::table.find(self.id)).set(self).execute(connection);
    }

    pub fn lookup_by_device(connection: &PgConnection, device: &Device) -> Option<WeathermapDeviceInfo> {
        match weathermap_device_infos::table
            .filter(weathermap_device_infos::device_id.eq(device.id))
            .first::<WeathermapDeviceInfo>(connection)
        {
            Ok(weathermap_device_info) => {
                return Some(weathermap_device_info);
            },
            Err(_) => {
                return None;
            }
        }
    }

    pub fn delete(self: &WeathermapDeviceInfo, connection: &PgConnection) -> Result<usize, diesel::result::Error> {
        return diesel::delete(weathermap_device_infos::table.find(self.id)).execute(connection);
    }

    pub fn update_by_fqdn_or_create(connection: &PgConnection, fqdn: &String, updated_info: UpdatedWeathermapDeviceInfo) -> Result<WeathermapDeviceInfo, String> {
        if let Some(device) = Device::find_by_fqdn(connection, fqdn) {
            let mut wmap_info;
            if let Some(weathermap_info) = WeathermapDeviceInfo::lookup_by_device(connection, &device) {
                wmap_info = weathermap_info;
                wmap_info.x = updated_info.x;
                wmap_info.y = updated_info.y;
                wmap_info.expanded_by_default = updated_info.expanded_by_default;
                wmap_info.super_node = updated_info.super_node;

                if let Ok(_) = wmap_info.update(connection) {
                    return Ok(wmap_info);
                } else {
                    return Err("failed to update WeathermapDeviceInfo".to_string());
                }
            } else {
                let template = NewWeathermapDeviceInfo {
                    x: updated_info.x,
                    y: updated_info.y,
                    expanded_by_default: updated_info.expanded_by_default,
                    super_node: updated_info.super_node,
                    device_id: device.id,
                };
                if let Ok(wmap_created_object) = WeathermapDeviceInfo::create(&template, connection) {
                    return Ok(wmap_created_object);
                } else {
                    return Err("couldn't create WeathermapDeviceInfo".to_string());
                }
            }
        } else {
            return Err("device not found".to_string());
        }
    }

    pub fn device(self: &WeathermapDeviceInfo, connection: &PgConnection) -> Device {
        return Device::by_id(self.device_id, connection).unwrap();
    }
}
