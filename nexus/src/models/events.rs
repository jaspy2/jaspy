use std::collections::{HashMap, HashSet};

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
    old_state: bool,
    new_state: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceOSInfoChangedEvent {
    fqdn: String,
    old_state: String,
    new_state: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceBaseMACChangedEvent {
    fqdn: String,
    old_state: String,
    new_state: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCreatedEvent {
    fqdn: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub event_type: String,

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
    device_created_event: Option<DeviceCreatedEvent>,
}

impl Event {
    pub fn new_empty(event_type: &str) -> Event {
        let event = Event {
            event_type: event_type.to_string(),
            ping_change: None,
            interface_up_down: None,
            interface_speed: None,
            device_polling_changed: None,
            device_os_info_changed: None,
            device_base_mac_changed: None,
            device_created_event: None,
        };

        return event;
    }

    pub fn device_created_event(fqdn: &String) -> Event {
        let mut event = Event::new_empty("deviceCreated");
        event.device_created_event = Some(DeviceCreatedEvent {
            fqdn: fqdn.clone(),
        });
        return event;
    }

    pub fn device_polling_changed_event(fqdn: &String, old_state: bool, new_state: bool) -> Event {
        let mut event = Event::new_empty("devicePollingChanged");
        event.device_polling_changed = Some(DevicePollingChangedEvent {
            fqdn: fqdn.clone(),
            old_state: old_state,
            new_state: new_state,
        });
        return event;
    }

    pub fn device_os_info_changed_event(fqdn: &String, old_state: &String, new_state: &String) -> Event {
        let mut event = Event::new_empty("deviceOsInfoChanged");
        event.device_os_info_changed = Some(DeviceOSInfoChangedEvent {
            fqdn: fqdn.clone(),
            old_state: old_state.clone(),
            new_state: new_state.clone(),
        });
        return event;
    }

    pub fn device_base_mac_changed_event(fqdn: &String, old_state: &String, new_state: &String) -> Event {
        let mut event = Event::new_empty("deviceOsInfoChanged");
        event.device_base_mac_changed = Some(DeviceBaseMACChangedEvent {
            fqdn: fqdn.clone(),
            old_state: old_state.clone(),
            new_state: new_state.clone(),
        });
        return event;
    }

    pub fn ping_change_event(fqdn: &String, neighbors: HashSet<String>, old_state: bool, new_state: bool) -> Event {
        let mut event = Event::new_empty("pingChange");

        let mut pce = PingChangeEvent {
            fqdn: fqdn.clone(),
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
            fqdn: fqdn.clone(),
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
            fqdn: fqdn.clone(),
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