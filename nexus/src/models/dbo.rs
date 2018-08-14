use schema::{devices,interfaces};
use diesel;
use diesel::pg::PgConnection;
use diesel::BelongingToDsl;
use diesel::RunQueryDsl;
use diesel::QueryDsl;
use diesel::ExpressionMethods;
use diesel::BoolExpressionMethods;

#[table_name = "devices"]
#[derive(Insertable)]
pub struct NewDevice {
    pub name: String,
    pub dns_domain: String,
    pub snmp_community: Option<String>,
    pub base_mac: Option<String>,
    pub polling_enabled: Option<bool>,
    pub os_info: Option<String>,
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
pub struct Device {
    pub id: i32,
    pub name: String,
    pub dns_domain: String,
    pub snmp_community: Option<String>,
    pub base_mac: Option<String>,
    pub polling_enabled: Option<bool>,
    pub os_info: Option<String>
}

#[belongs_to(Device)]
#[table_name = "interfaces"]
#[derive(Serialize, Deserialize, Queryable, Identifiable, AsChangeset, Associations, Clone)]
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

    pub fn create(new_device: &NewDevice, connection: &PgConnection) -> Result<Device, diesel::result::Error> {
        let result = diesel::insert_into(devices::table)
            .values(new_device)
            .get_result(connection);
        return result;
    }

    pub fn find_by_fqdn(connection: &PgConnection, hostname: &String, domain_name: &String) -> Option<Device> {
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

    pub fn update(self: &Device, connection: &PgConnection) -> Result<usize, diesel::result::Error> {
        return diesel::update(devices::table.find(self.id)).set(self).execute(connection);
    }

    pub fn delete(self: &Device, connection: &PgConnection) -> Result<usize, diesel::result::Error> {
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

    pub fn create(new_interface: &NewInterface, connection: &PgConnection) -> Result<Interface, diesel::result::Error> {
        let result = diesel::insert_into(interfaces::table)
            .values(new_interface)
            .get_result(connection);
        return result;
    }

    pub fn update(self: &Interface, connection: &PgConnection) -> Result<usize, diesel::result::Error> {
        return diesel::update(interfaces::table.find(self.id)).set(self).execute(connection);
    }

    pub fn delete(self: &Interface, connection: &PgConnection) -> Result<usize, diesel::result::Error> {
        return diesel::delete(interfaces::table.find(self.id)).execute(connection);
    }

    pub fn peer_interface(self: &Interface, connection: &PgConnection) -> Option<Interface> {
        match self.connected_interface {
            Some(connected_interface_id) => {
                match Interface::by_id(connected_interface_id, connection) {
                    Some(peer_interface) => {
                        return Some(peer_interface);
                    },
                    None => {
                        // TODO: WTF, this cant happen, probably?
                        return None;
                    }
                }
            },
            None => {
                return None;
            }
        }
    }

    pub fn device(self: &Interface, connection: &PgConnection) -> Device {
        return Device::by_id(self.device_id, connection).unwrap();
    }
}