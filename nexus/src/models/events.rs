use std::collections::{HashMap, HashSet};
use crate::utilities::tools::{get_time};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PingChangeEvent {
    fqdn: String,
    
    neighbors: Vec<String>,

    old_state: bool,
    new_state: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterfaceUpDownEvent {
    fqdn: String,
    name: String,

    neighbor: Option<String>,
    neighbor_name: Option<String>,
    neighbor_links_state: HashMap<String,String>,

    old_state: bool,
    new_state: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterfaceSpeedEvent {
    fqdn: String,
    name: String,

    neighbor: Option<String>,
    neighbor_name: Option<String>,

    old_state: i32,
    new_state: i32,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DevicePollingChangedEvent {
    fqdn: String,
    old_state: Option<bool>,
    new_state: Option<bool>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceOSInfoChangedEvent {
    fqdn: String,
    old_state: Option<String>,
    new_state: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceBaseMACChangedEvent {
    fqdn: String,
    old_state: Option<String>,
    new_state: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCreatedEvent {
    fqdn: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceDeletedEvent {
    fqdn: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub event_type: String,
    pub create_time: f64,

    #[serde(skip_serializing_if="Option::is_none")]
    ping_change: Option<PingChangeEvent>,

    #[serde(skip_serializing_if="Option::is_none")]
    interface_up_down: Option<InterfaceUpDownEvent>,

    #[serde(skip_serializing_if="Option::is_none")]
    interface_speed: Option<InterfaceSpeedEvent>,

    #[serde(skip_serializing_if="Option::is_none")]
    device_polling_changed: Option<DevicePollingChangedEvent>,

    #[serde(skip_serializing_if="Option::is_none")]
    device_os_info_changed: Option<DeviceOSInfoChangedEvent>,

    #[serde(skip_serializing_if="Option::is_none")]
    device_base_mac_changed: Option<DeviceBaseMACChangedEvent>,

    #[serde(skip_serializing_if="Option::is_none")]
    device_created: Option<DeviceCreatedEvent>,

    #[serde(skip_serializing_if="Option::is_none")]
    device_deleted: Option<DeviceDeletedEvent>,
}

impl Event {
    // The device this event concerns; every payload variant carries an fqdn.
    // Used to route events onto per-device live-update topics (livelog).
    pub fn fqdn(&self) -> Option<&str> {
        if let Some(ref e) = self.ping_change { return Some(&e.fqdn); }
        if let Some(ref e) = self.interface_up_down { return Some(&e.fqdn); }
        if let Some(ref e) = self.interface_speed { return Some(&e.fqdn); }
        if let Some(ref e) = self.device_polling_changed { return Some(&e.fqdn); }
        if let Some(ref e) = self.device_os_info_changed { return Some(&e.fqdn); }
        if let Some(ref e) = self.device_base_mac_changed { return Some(&e.fqdn); }
        if let Some(ref e) = self.device_created { return Some(&e.fqdn); }
        if let Some(ref e) = self.device_deleted { return Some(&e.fqdn); }
        None
    }

    pub fn new_empty(event_type: &str) -> Event {
        let event = Event {
            event_type: event_type.to_string(),
            create_time: get_time(),
            ping_change: None,
            interface_up_down: None,
            interface_speed: None,
            device_polling_changed: None,
            device_os_info_changed: None,
            device_base_mac_changed: None,
            device_created: None,
            device_deleted: None,
        };

        return event;
    }

    pub fn device_created_event(fqdn: &str) -> Event {
        let mut event = Event::new_empty("deviceCreated");
        event.device_created = Some(DeviceCreatedEvent {
            fqdn: fqdn.to_string(),
        });
        return event;
    }

    pub fn device_deleted_event(fqdn: &str) -> Event {
        let mut event = Event::new_empty("deviceDeleted");
        event.device_deleted = Some(DeviceDeletedEvent {
            fqdn: fqdn.to_string(),
        });
        return event;
    }

    pub fn device_polling_changed_event(fqdn: &str, old_state: Option<bool>, new_state: Option<bool>) -> Event {
        let mut event = Event::new_empty("devicePollingChanged");
        event.device_polling_changed = Some(DevicePollingChangedEvent {
            fqdn: fqdn.to_string(),
            old_state: old_state,
            new_state: new_state,
        });
        return event;
    }

    pub fn device_os_info_changed_event(fqdn: &str, old_state: &Option<String>, new_state: &Option<String>) -> Event {
        let mut event = Event::new_empty("deviceOsInfoChanged");
        event.device_os_info_changed = Some(DeviceOSInfoChangedEvent {
            fqdn: fqdn.to_string(),
            old_state: old_state.clone(),
            new_state: new_state.clone(),
        });
        return event;
    }

    pub fn device_base_mac_changed_event(fqdn: &str, old_state: &Option<String>, new_state: &Option<String>) -> Event {
        let mut event = Event::new_empty("deviceBaseMacChanged");
        event.device_base_mac_changed = Some(DeviceBaseMACChangedEvent {
            fqdn: fqdn.to_string(),
            old_state: old_state.clone(),
            new_state: new_state.clone(),
        });
        return event;
    }

    pub fn ping_change_event(fqdn: &String, neighbors: HashSet<String>, old_state: bool, new_state: bool) -> Event {
        let mut event = Event::new_empty("pingChange");

        let mut pce = PingChangeEvent {
            fqdn: fqdn.to_string(),
            neighbors: Vec::new(),
            old_state: old_state,
            new_state: new_state,
        };

        for nei in neighbors.iter() {
            pce.neighbors.push(nei.clone());
        }

        pce.neighbors.sort();

        event.ping_change = Some(pce);

        return event;
    }

    pub fn interface_updown_event(fqdn: &String, name: &String, neighbor: Option<String>, neighbor_name: Option<String>, link_statuses: &HashMap<String, String>, old_state: bool, new_state: bool) -> Event {
        let mut event = Event::new_empty("interfaceUpDown");

        let ifude = InterfaceUpDownEvent {
            fqdn: fqdn.to_string(),
            name: name.clone(),
            neighbor: neighbor,
            neighbor_name: neighbor_name,
            neighbor_links_state: link_statuses.clone(),
            old_state: old_state,
            new_state: new_state,
        };

        event.interface_up_down = Some(ifude);

        return event;
    }

    pub fn interface_speed_event(fqdn: &String, name: &String, neighbor: Option<String>, neighbor_name: Option<String>, old_state: i32, new_state: i32) -> Event {
        let mut event = Event::new_empty("interfaceSpeed");

        let ifse = InterfaceSpeedEvent {
            fqdn: fqdn.to_string(),
            name: name.clone(),
            neighbor: neighbor,
            neighbor_name: neighbor_name,
            old_state: old_state,
            new_state: new_state,
        };

        event.interface_speed = Some(ifse);

        return event;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_set_event_type_and_fqdn() {
        let fqdn = "sw1.example.com".to_string();
        let cases: Vec<(Event, &str)> = vec![
            (Event::device_created_event(&fqdn), "deviceCreated"),
            (Event::device_deleted_event(&fqdn), "deviceDeleted"),
            (Event::device_polling_changed_event(&fqdn, Some(false), Some(true)), "devicePollingChanged"),
            (Event::device_os_info_changed_event(&fqdn, &None, &Some("IOS 15.2".to_string())), "deviceOsInfoChanged"),
            (Event::device_base_mac_changed_event(&fqdn, &None, &Some("aa:bb:cc:dd:ee:ff".to_string())), "deviceBaseMacChanged"),
            (Event::ping_change_event(&fqdn, std::collections::HashSet::new(), false, true), "pingChange"),
            (Event::interface_updown_event(&fqdn, &"Ethernet1/1".to_string(), None, None, &HashMap::new(), false, true), "interfaceUpDown"),
            (Event::interface_speed_event(&fqdn, &"Ethernet1/1".to_string(), None, None, 100, 1000), "interfaceSpeed"),
        ];
        for (event, expected_type) in cases {
            assert_eq!(event.event_type, expected_type);
            assert_eq!(event.fqdn(), Some(fqdn.as_str()), "fqdn missing for {}", expected_type);
        }
    }

    #[test]
    fn new_empty_has_no_fqdn() {
        assert_eq!(Event::new_empty("somethingElse").fqdn(), None);
    }

    #[test]
    fn ping_change_sorts_neighbors() {
        let mut neighbors = HashSet::new();
        neighbors.insert("bravo.example.com".to_string());
        neighbors.insert("alpha.example.com".to_string());
        let event = Event::ping_change_event(&"sw1.example.com".to_string(), neighbors, true, false);
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(
            json["pingChange"]["neighbors"],
            serde_json::json!(["alpha.example.com", "bravo.example.com"])
        );
    }

    #[test]
    fn event_json_uses_camel_case_and_omits_absent_variants() {
        let mut link_statuses = HashMap::new();
        link_statuses.insert("Ethernet1/2".to_string(), "up".to_string());
        let event = Event::interface_updown_event(
            &"sw1.example.com".to_string(),
            &"Ethernet1/1".to_string(),
            Some("sw2.example.com".to_string()),
            Some("Ethernet2/1".to_string()),
            &link_statuses,
            true,
            false,
        );
        let json = serde_json::to_value(&event).unwrap();
        let obj = json.as_object().unwrap();
        assert_eq!(obj["eventType"], "interfaceUpDown");
        let payload = obj["interfaceUpDown"].as_object().unwrap();
        assert_eq!(payload["oldState"], true);
        assert_eq!(payload["newState"], false);
        assert_eq!(payload["neighborName"], "Ethernet2/1");
        assert_eq!(payload["neighborLinksState"]["Ethernet1/2"], "up");
        // skip_serializing_if drops every unused variant.
        assert!(!obj.contains_key("pingChange"));
        assert!(!obj.contains_key("deviceCreated"));
        assert!(!obj.contains_key("interfaceSpeed"));
    }
}
