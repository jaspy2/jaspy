use crate::schema::{devices,interfaces,weathermap_device_infos,client_locations,settings,issue_acks};
use diesel;
use crate::db::AnyConnection;
use diesel::prelude::*;

#[derive(Insertable, Serialize, Deserialize)]
#[diesel(table_name = devices)]
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

#[derive(Insertable)]
#[diesel(table_name = interfaces)]
pub struct NewInterface {
    pub index: i32,
    pub interface_type: String,
    pub device_id: i32,
    pub name: String,
    pub alias: Option<String>,
    pub description: Option<String>,
    pub media: Option<String>,
}

#[derive(Serialize, Deserialize, Queryable, Identifiable, AsChangeset, Clone)]
#[diesel(table_name = devices, treat_none_as_null = true)]
#[serde(rename_all = "camelCase")]
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

#[derive(Insertable)]
#[diesel(table_name = weathermap_device_infos)]
pub struct NewWeathermapDeviceInfo {
    pub x: f64,
    pub y: f64,
    pub super_node: bool,
    pub expanded_by_default: bool,
    pub device_id: i32,
}

#[derive(Insertable)]
#[diesel(table_name = client_locations)]
pub struct NewClientLocation {
    pub device_id: i32,
    pub ip_address: String,
    pub port_info: String,
    pub hw_address: String,
}

#[derive(Serialize, Deserialize, Queryable, Identifiable, AsChangeset, Associations, Clone)]
#[diesel(table_name = client_locations, belongs_to(Device))]
#[serde(rename_all = "camelCase")]
pub struct ClientLocation {
    pub id: i32,
    pub device_id: i32,
    pub ip_address: String,
    pub port_info: String,
    pub hw_address: String,
}

#[derive(Serialize, Deserialize, Queryable, Identifiable, AsChangeset, Associations, Clone)]
#[diesel(table_name = weathermap_device_infos, belongs_to(Device))]
#[serde(rename_all = "camelCase")]
pub struct WeathermapDeviceInfo {
    pub id: i32,
    pub x: f64,
    pub y: f64,
    pub super_node: bool,
    pub expanded_by_default: bool,
    pub device_id: i32,
}

#[derive(Serialize, Deserialize, Queryable, Identifiable, AsChangeset, Associations, Clone)]
#[diesel(table_name = interfaces, belongs_to(Device), treat_none_as_null = true)]
#[serde(rename_all = "camelCase")]
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
    // Physical media / form-factor derived from ENTITY-MIB: "copper" (fixed
    // RJ45 port), "sfp" (empty SFP cage) or "sfp: <descr>" (populated
    // transceiver). None until discovery reads it. See collectors::entity_media.
    pub media: Option<String>,
}

// Simple key/value store for runtime-mutable state that must survive restarts
// (e.g. the discovery engine config and the current event name).
#[derive(Serialize, Deserialize, Queryable, Insertable, Identifiable, AsChangeset, Clone)]
#[diesel(table_name = settings, primary_key(name))]
#[serde(rename_all = "camelCase")]
pub struct Setting {
    pub name: String,
    pub value: String,
}

impl Setting {
    pub fn get(connection: &mut AnyConnection, name: &str) -> Option<String> {
        match settings::table
            .filter(settings::name.eq(name))
            .first::<Setting>(connection)
        {
            Ok(setting) => {
                return Some(setting.value);
            },
            Err(_) => {
                return None;
            }
        }
    }

    pub fn set(connection: &mut AnyConnection, name: &str, value: &str) -> Result<usize, diesel::result::Error> {
        let setting = Setting { name: name.to_string(), value: value.to_string() };
        // ON CONFLICT is not expressible through the MultiConnection enum;
        // both concrete backends support it, so dispatch per variant.
        crate::with_backend!(connection, |conn| {
            diesel::insert_into(settings::table)
                .values(&setting)
                .on_conflict(settings::name)
                .do_update()
                .set(settings::value.eq(value))
                .execute(conn)
        })
    }

    pub fn delete(connection: &mut AnyConnection, name: &str) -> Result<usize, diesel::result::Error> {
        return diesel::delete(settings::table.filter(settings::name.eq(name))).execute(connection);
    }
}

// A persisted acknowledgement of one issue occurrence. Issues are derived, not
// stored (see utilities::issues); this table only remembers which occurrence an
// operator has silenced. `first_seen` is the tracker's onset timestamp for the
// acked occurrence — the GET /issues handler only treats an issue as
// acknowledged when both the key and first_seen still match, so a
// cleared-then-recurring condition (new first_seen) re-alerts.
#[derive(Serialize, Deserialize, Queryable, Insertable, Identifiable, AsChangeset, Clone, Debug)]
#[diesel(table_name = issue_acks, primary_key(issue_key))]
#[serde(rename_all = "camelCase")]
pub struct IssueAck {
    pub issue_key: String,
    pub first_seen: i64,
    pub acked_at: i64,
    pub acked_by: Option<String>,
    pub note: Option<String>,
}

impl IssueAck {
    pub fn all(connection: &mut AnyConnection) -> Vec<IssueAck> {
        issue_acks::table.load::<IssueAck>(connection).unwrap_or_default()
    }

    // Upsert: acking an already-acked issue (e.g. after it recurred with a new
    // first_seen) overwrites the previous ack. ON CONFLICT is not expressible
    // through the MultiConnection enum; dispatch per backend like Setting::set.
    pub fn upsert(&self, connection: &mut AnyConnection) -> Result<usize, diesel::result::Error> {
        crate::with_backend!(connection, |conn| {
            diesel::insert_into(issue_acks::table)
                .values(self)
                .on_conflict(issue_acks::issue_key)
                .do_update()
                .set((
                    issue_acks::first_seen.eq(self.first_seen),
                    issue_acks::acked_at.eq(self.acked_at),
                    issue_acks::acked_by.eq(&self.acked_by),
                    issue_acks::note.eq(&self.note),
                ))
                .execute(conn)
        })
    }

    pub fn delete(connection: &mut AnyConnection, issue_key: &str) -> Result<usize, diesel::result::Error> {
        diesel::delete(issue_acks::table.filter(issue_acks::issue_key.eq(issue_key))).execute(connection)
    }

    // Drop ack rows whose issue is no longer active, keeping the table bounded.
    // `active_keys` is every issue_key present in the current derivation.
    pub fn delete_orphans(connection: &mut AnyConnection, active_keys: &std::collections::HashSet<String>) -> Result<usize, diesel::result::Error> {
        let mut removed = 0;
        for ack in IssueAck::all(connection) {
            if !active_keys.contains(&ack.issue_key) {
                removed += IssueAck::delete(connection, &ack.issue_key)?;
            }
        }
        Ok(removed)
    }
}

impl ClientLocation {
    pub fn all(connection: &mut AnyConnection) -> Vec<ClientLocation> {
        match client_locations::table.load(connection) {
            Ok(result) => {
                return result;
            },
            Err(_) => {
                return Vec::new();
            }
        }
    }

    pub fn create(new_client_location: &NewClientLocation, connection: &mut AnyConnection) -> Result<ClientLocation, diesel::result::Error> {
        // INSERT..RETURNING (get_result) needs per-variant dispatch; see Setting::set.
        crate::with_backend!(connection, |conn| {
            diesel::insert_into(client_locations::table)
                .values(new_client_location)
                .get_result(conn)
        })
    }

    pub fn update(self: &ClientLocation, connection: &mut AnyConnection) -> Result<usize, diesel::result::Error> {
        return diesel::update(client_locations::table.find(self.id)).set(self).execute(connection);
    }

    pub fn delete(self: &ClientLocation, connection: &mut AnyConnection) -> Result<usize, diesel::result::Error> {
        return diesel::delete(client_locations::table.find(self.id)).execute(connection);
    }

    pub fn by_ip(ip_address: &String, connection: &mut AnyConnection) -> Option<ClientLocation> {
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
    pub fn by_id(id: i32, connection: &mut AnyConnection) -> Option<Device> {
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

    pub fn by_base_mac(base_mac: &String, connection: &mut AnyConnection) -> Option<Device> {
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

    pub fn all(connection: &mut AnyConnection) -> Vec<Device> {
        match devices::table.load(connection) {
            Ok(result) => {
                return result;
            },
            Err(_) => {
                return Vec::new();
            }
        }
    }

    pub fn monitored(connection: &mut AnyConnection) -> Vec<Device> {
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

    pub fn interface_by_name(self: &Device, connection: &mut AnyConnection, name: &String) -> Option<Interface> {
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

    pub fn interface_by_index(self: &Device, connection: &mut AnyConnection, index: &i32) -> Option<Interface> {
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

    pub fn create(new_device: &NewDevice, connection: &mut AnyConnection) -> Result<Device, diesel::result::Error> {
        // INSERT..RETURNING (get_result) needs per-variant dispatch; see Setting::set.
        crate::with_backend!(connection, |conn| {
            diesel::insert_into(devices::table)
                .values(new_device)
                .get_result(conn)
        })
    }

    pub fn find_by_hostname_and_domain_name(connection: &mut AnyConnection, hostname: &String, domain_name: &String) -> Option<Device> {
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

    pub fn find_by_fqdn(connection: &mut AnyConnection, fqdn: &str) -> Option<Device> {
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

    pub fn update(self: &Device, connection: &mut AnyConnection) -> Result<usize, diesel::result::Error> {
        return diesel::update(devices::table.find(self.id)).set(self).execute(connection);
    }

    pub fn delete(self: &Device, connection: &mut AnyConnection) -> Result<usize, diesel::result::Error> {
        if let Ok(weathermap_device_info) = WeathermapDeviceInfo::belonging_to(self).load(connection) {
            let wmdis : Vec<WeathermapDeviceInfo> = weathermap_device_info;
            for wmdi in wmdis.iter() {
                if let Err(_) = wmdi.delete(connection) {
                    // TODO: log
                }
            }
        }

        for interface in self.interfaces(connection).iter() {
            if let Err(d) = interface.delete(connection) {
                // TODO: log
                println!("{}", d);
            }
        }

        if let Ok(client_location_infos) = ClientLocation::belonging_to(self).load(connection) {
            let cl_info_vec: Vec<ClientLocation> = client_location_infos;
            for client_location_info in cl_info_vec.iter() {
                if let Err(d) = client_location_info.delete(connection) {
                    println!("{}", d);
                }
            }
        }

        return diesel::delete(devices::table.find(self.id)).execute(connection);
    }

    pub fn interfaces(self: &Device, connection: &mut AnyConnection) -> Vec<Interface> {
        match Interface::belonging_to(self).load(connection) {
            Ok(result) => {
                return result;
            },
            Err(_) => {
                return Vec::new();
            }
        }
    }

    pub fn weathermap_info(self: &Device, connection: &mut AnyConnection) -> Option<WeathermapDeviceInfo> {
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
    pub fn by_id(id: i32, connection: &mut AnyConnection) -> Option<Interface> {
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

    pub fn all(connection: &mut AnyConnection) -> Vec<Interface> {
        match interfaces::table.load(connection) {
            Ok(result) => {
                return result;
            },
            Err(_) => {
                return Vec::new();
            }
        }
    }

    pub fn create(new_interface: &NewInterface, connection: &mut AnyConnection) -> Result<Interface, diesel::result::Error> {
        // INSERT..RETURNING (get_result) needs per-variant dispatch; see Setting::set.
        crate::with_backend!(connection, |conn| {
            diesel::insert_into(interfaces::table)
                .values(new_interface)
                .get_result(conn)
        })
    }

    pub fn name(self: &Interface) -> String {
        if let Some(ref display_name) = self.display_name {
            return display_name.clone();
        } else {
            return self.name.clone();
        }
    }

    pub fn update(self: &Interface, connection: &mut AnyConnection) -> Result<usize, diesel::result::Error> {
        return diesel::update(interfaces::table.find(self.id)).set(self).execute(connection);
    }

    pub fn delete(self: &Interface, connection: &mut AnyConnection) -> Result<usize, diesel::result::Error> {
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

    // Interfaces on other devices whose connected_interface/virtual_connection
    // points at one of `ids`. Links are stored one-directionally, so showing a
    // device's links needs the reverse direction too.
    pub fn pointing_at(connection: &mut AnyConnection, ids: &Vec<i32>) -> Vec<Interface> {
        interfaces::table
            .filter(
                interfaces::connected_interface.eq_any(ids.iter().map(|id| Some(*id)).collect::<Vec<Option<i32>>>())
                .or(interfaces::virtual_connection.eq_any(ids.iter().map(|id| Some(*id)).collect::<Vec<Option<i32>>>()))
            )
            .load::<Interface>(connection)
            .unwrap_or_default()
    }

    pub fn peer_interface(self: &Interface, connection: &mut AnyConnection) -> Option<Interface> {
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

    pub fn device(self: &Interface, connection: &mut AnyConnection) -> Device {
        return Device::by_id(self.device_id, connection).unwrap();
    }
}

impl WeathermapDeviceInfo {
    pub fn create(new_weathermap_device_info: &NewWeathermapDeviceInfo, connection: &mut AnyConnection) -> Result<WeathermapDeviceInfo, diesel::result::Error> {
        // INSERT..RETURNING (get_result) needs per-variant dispatch; see Setting::set.
        crate::with_backend!(connection, |conn| {
            diesel::insert_into(weathermap_device_infos::table)
                .values(new_weathermap_device_info)
                .get_result(conn)
        })
    }

    pub fn update(self: &WeathermapDeviceInfo, connection: &mut AnyConnection) -> Result<usize, diesel::result::Error> {
        return diesel::update(weathermap_device_infos::table.find(self.id)).set(self).execute(connection);
    }

    pub fn lookup_by_device(connection: &mut AnyConnection, device: &Device) -> Option<WeathermapDeviceInfo> {
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

    pub fn delete(self: &WeathermapDeviceInfo, connection: &mut AnyConnection) -> Result<usize, diesel::result::Error> {
        return diesel::delete(weathermap_device_infos::table.find(self.id)).execute(connection);
    }

    pub fn update_by_fqdn_or_create(connection: &mut AnyConnection, fqdn: &String, updated_info: UpdatedWeathermapDeviceInfo) -> Result<WeathermapDeviceInfo, String> {
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

    pub fn device(self: &WeathermapDeviceInfo, connection: &mut AnyConnection) -> Device {
        return Device::by_id(self.device_id, connection).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diesel::Connection;

    // Real-database unit tests against in-memory sqlite: exercises the
    // AnyConnection dispatch sites (upsert, INSERT..RETURNING) and doubles as
    // the drift guard between migrations_sqlite/ and schema.rs (every column
    // of every table is selected through the Queryable structs).
    //
    // One direct connection, never a pool: each pooled :memory: connection
    // would be a separate empty database.
    fn conn() -> AnyConnection {
        let mut connection = AnyConnection::Sqlite(
            diesel::sqlite::SqliteConnection::establish(":memory:").unwrap(),
        );
        crate::db::run_migrations(&mut connection).unwrap();
        connection
    }

    fn new_device(name: &str, base_mac: Option<&str>) -> NewDevice {
        NewDevice {
            name: name.to_string(),
            dns_domain: "test.example".to_string(),
            snmp_community: Some("public".to_string()),
            base_mac: base_mac.map(String::from),
            polling_enabled: None,
            os_info: Some("Test OS".to_string()),
            device_type: Some("T-1000".to_string()),
            software_version: Some("1.0".to_string()),
        }
    }

    #[test]
    fn setting_upsert_roundtrip() {
        let mut conn = conn();
        assert_eq!(Setting::get(&mut conn, "event"), None);
        Setting::set(&mut conn, "event", "first").unwrap();
        assert_eq!(Setting::get(&mut conn, "event").as_deref(), Some("first"));
        // Upsert overwrites (the on_conflict dispatch site).
        Setting::set(&mut conn, "event", "second").unwrap();
        assert_eq!(Setting::get(&mut conn, "event").as_deref(), Some("second"));
        Setting::delete(&mut conn, "event").unwrap();
        assert_eq!(Setting::get(&mut conn, "event"), None);
    }

    #[test]
    fn issue_ack_roundtrip_and_orphan_cleanup() {
        let mut conn = conn();
        assert!(IssueAck::all(&mut conn).is_empty());

        let ack = IssueAck {
            issue_key: "sw1.test.example|iface-flapping|10001".to_string(),
            first_seen: 1000,
            acked_at: 2000,
            acked_by: None,
            note: Some("known cabling work".to_string()),
        };
        ack.upsert(&mut conn).unwrap();
        assert_eq!(IssueAck::all(&mut conn).len(), 1);

        // Upsert with a new first_seen (occurrence recurred) overwrites in place.
        let mut ack2 = ack.clone();
        ack2.first_seen = 5000;
        ack2.acked_at = 6000;
        ack2.upsert(&mut conn).unwrap();
        let rows = IssueAck::all(&mut conn);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].first_seen, 5000);

        // Orphan cleanup removes acks whose issue is no longer active.
        let active: std::collections::HashSet<String> =
            vec!["other.test.example|device-down|".to_string()].into_iter().collect();
        assert_eq!(IssueAck::delete_orphans(&mut conn, &active).unwrap(), 1);
        assert!(IssueAck::all(&mut conn).is_empty());
    }

    #[test]
    fn device_create_returns_row_and_lookups_work() {
        let mut conn = conn();
        let device = Device::create(&new_device("sw1", Some("aa:bb:cc:dd:ee:ff")), &mut conn).unwrap();
        assert!(device.id > 0, "INSERT..RETURNING must yield the generated id");
        assert_eq!(device.name, "sw1");
        assert_eq!(device.device_type.as_deref(), Some("T-1000"));

        assert_eq!(Device::find_by_fqdn(&mut conn, "sw1.test.example").unwrap().id, device.id);
        assert_eq!(Device::by_base_mac(&"aa:bb:cc:dd:ee:ff".to_string(), &mut conn).unwrap().id, device.id);
        assert!(Device::find_by_fqdn(&mut conn, "ghost.test.example").is_none());
        assert_eq!(Device::all(&mut conn).len(), 1);
        assert_eq!(Device::monitored(&mut conn).len(), 1); // polling_enabled None counts as monitored
    }

    #[test]
    fn interface_create_link_and_peer_lookup() {
        let mut conn = conn();
        let sw1 = Device::create(&new_device("sw1", None), &mut conn).unwrap();
        let sw2 = Device::create(&new_device("sw2", None), &mut conn).unwrap();
        let if1 = Interface::create(&NewInterface {
            index: 10101, interface_type: "ethernetCsmacd".to_string(), device_id: sw1.id,
            name: "Gi0/1".to_string(), alias: None, description: None, media: Some("copper".to_string()),
        }, &mut conn).unwrap();
        let mut if2 = Interface::create(&NewInterface {
            index: 10101, interface_type: "ethernetCsmacd".to_string(), device_id: sw2.id,
            name: "Gi0/1".to_string(), alias: None, description: None, media: None,
        }, &mut conn).unwrap();

        if2.connected_interface = Some(if1.id);
        if2.update(&mut conn).unwrap();

        let peer = Interface::by_id(if2.id, &mut conn).unwrap().peer_interface(&mut conn).unwrap();
        assert_eq!(peer.id, if1.id);
        assert_eq!(peer.device(&mut conn).id, sw1.id);
        assert_eq!(sw1.interfaces(&mut conn).len(), 1);
        // media round-trips through the new column (doubles as the sqlite
        // migration drift guard for it).
        assert_eq!(Interface::by_id(if1.id, &mut conn).unwrap().media.as_deref(), Some("copper"));
        assert_eq!(Interface::by_id(if2.id, &mut conn).unwrap().media, None);
    }

    #[test]
    fn client_location_create_and_unique_ip() {
        let mut conn = conn();
        let device = Device::create(&new_device("sw1", None), &mut conn).unwrap();
        let created = ClientLocation::create(&NewClientLocation {
            device_id: device.id,
            ip_address: "10.0.0.1".to_string(),
            port_info: "1/1".to_string(),
            hw_address: "02:aa:00:00:00:01".to_string(),
        }, &mut conn).unwrap();
        assert!(created.id > 0);
        assert_eq!(ClientLocation::by_ip(&"10.0.0.1".to_string(), &mut conn).unwrap().id, created.id);

        // Unique index on ip_address must hold on sqlite too.
        let duplicate = ClientLocation::create(&NewClientLocation {
            device_id: device.id,
            ip_address: "10.0.0.1".to_string(),
            port_info: "1/2".to_string(),
            hw_address: "02:aa:00:00:00:02".to_string(),
        }, &mut conn);
        assert!(duplicate.is_err(), "duplicate client ip must violate the unique index");
        assert_eq!(ClientLocation::all(&mut conn).len(), 1);
    }

    #[test]
    fn weathermap_info_create_and_update() {
        let mut conn = conn();
        let device = Device::create(&new_device("sw1", None), &mut conn).unwrap();
        let info = WeathermapDeviceInfo::create(&NewWeathermapDeviceInfo {
            x: 1.5, y: 2.5, super_node: false, expanded_by_default: true, device_id: device.id,
        }, &mut conn).unwrap();
        assert!(info.id > 0);
        assert_eq!(info.x, 1.5);

        let updated = WeathermapDeviceInfo::update_by_fqdn_or_create(
            &mut conn,
            &"sw1.test.example".to_string(),
            UpdatedWeathermapDeviceInfo { x: 9.0, y: 8.0, super_node: true, expanded_by_default: false },
        ).unwrap();
        assert_eq!(updated.id, info.id);
        assert_eq!(updated.x, 9.0);
        assert!(updated.super_node);
        assert_eq!(WeathermapDeviceInfo::lookup_by_device(&mut conn, &device).unwrap().x, 9.0);
    }

    #[test]
    fn device_delete_cascades_dependents() {
        let mut conn = conn();
        let device = Device::create(&new_device("sw1", None), &mut conn).unwrap();
        Interface::create(&NewInterface {
            index: 1, interface_type: "ethernetCsmacd".to_string(), device_id: device.id,
            name: "Gi0/1".to_string(), alias: None, description: None, media: None,
        }, &mut conn).unwrap();
        ClientLocation::create(&NewClientLocation {
            device_id: device.id, ip_address: "10.0.0.1".to_string(),
            port_info: "1/1".to_string(), hw_address: "02:aa:00:00:00:01".to_string(),
        }, &mut conn).unwrap();

        device.delete(&mut conn).unwrap();
        assert!(Device::find_by_fqdn(&mut conn, "sw1.test.example").is_none());
        assert_eq!(ClientLocation::all(&mut conn).len(), 0);
    }
}
