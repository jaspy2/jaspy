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
pub struct Event {
    event_type: String,

    #[serde(skip_serializing_if="Option::is_none")]
    ping_change: Option<PingChangeEvent>,

    #[serde(skip_serializing_if="Option::is_none")]
    interface_up_down: Option<InterfaceUpDownEvent>,

    #[serde(skip_serializing_if="Option::is_none")]
    interface_speed: Option<InterfaceSpeedEvent>,
}

impl Event {
    pub fn ping_change_event(fqdn: &String, neighbors: HashSet<String>, old_state: bool, new_state: bool) -> Event {
        let mut event = Event {
            event_type: "pingChange".to_string(),
            ping_change: None,
            interface_up_down: None,
            interface_speed: None,
        };

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
        let mut event = Event {
            event_type: "interfaceUpDown".to_string(),
            ping_change: None,
            interface_up_down: None,
            interface_speed: None,
        };

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
        let mut event = Event {
            event_type: "interfaceSpeed".to_string(),
            ping_change: None,
            interface_up_down: None,
            interface_speed: None,
        };

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