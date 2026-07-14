use crate::models;
use crate::db;
use crate::utilities;
use rocket::{get, post, put, delete};
use rocket::serde::json::Json;
use std::sync::{Arc, Mutex};
use rocket::State;

const EVENT_SETTING: &str = "event";

// Live (up, seconds since last interface poll) for a device, from IMDS.
fn imds_device_live(imds: &State<Arc<Mutex<utilities::imds::IMDS>>>, fqdn: &str) -> (Option<bool>, Option<u64>) {
    if let Ok(ref mut imds) = imds.inner().lock() {
        if let Some(device_metric) = imds.get_device(fqdn) {
            let seconds_since_last_poll = if device_metric.last_poll > 0 {
                Some(utilities::tools::get_time_msecs().saturating_sub(device_metric.last_poll) / 1000)
            } else {
                None
            };
            return (device_metric.up, seconds_since_last_poll);
        }
    }
    (None, None)
}

fn imds_device_up(imds: &State<Arc<Mutex<utilities::imds::IMDS>>>, fqdn: &str) -> Option<bool> {
    imds_device_live(imds, fqdn).0
}

fn event_name(connection: &mut diesel::PgConnection) -> Option<String> {
    models::dbo::Setting::get(connection, EVENT_SETTING)
        .and_then(|json| serde_json::from_str::<models::json::ApiEvent>(&json).ok())
        .and_then(|event| event.name)
}

#[get("/summary")]
pub fn summary(
    mut connection: db::JaspyDB,
    imds: &State<Arc<Mutex<utilities::imds::IMDS>>>,
    runtime_info: &State<Arc<Mutex<models::internal::RuntimeInfo>>>,
    discovery_control: &State<Arc<Mutex<crate::collectors::discovery::DiscoveryControl>>>,
) -> Json<models::json::ApiSummary> {
    let devices = models::dbo::Device::all(&mut connection);
    let mut devices_up = 0;
    let mut devices_down = 0;
    let mut devices_unknown = 0;
    for device in devices.iter() {
        let fqdn = format!("{}.{}", device.name, device.dns_domain);
        match imds_device_up(imds, &fqdn) {
            Some(true) => devices_up += 1,
            Some(false) => devices_down += 1,
            None => devices_unknown += 1,
        }
    }

    let (state_id, startup_time) = match runtime_info.inner().lock() {
        Ok(rti) => (rti.state_id(), rti.startup_time),
        Err(_) => (0, 0.0),
    };
    let discovery = match discovery_control.inner().lock() {
        Ok(control) => control.status_dto(),
        Err(_) => models::json::DiscoveryStatus::default(),
    };

    Json(models::json::ApiSummary {
        version: env!("CARGO_PKG_VERSION").to_string(),
        state_id: state_id,
        startup_time: startup_time,
        event_name: event_name(&mut connection),
        device_count: devices.len() as u64,
        devices_up: devices_up,
        devices_down: devices_down,
        devices_unknown: devices_unknown,
        discovery: discovery,
    })
}

fn api_device(connection: &mut diesel::PgConnection, imds: &State<Arc<Mutex<utilities::imds::IMDS>>>, device: &models::dbo::Device) -> models::json::ApiDevice {
    let fqdn = format!("{}.{}", device.name, device.dns_domain);
    let (up, seconds_since_last_poll) = imds_device_live(imds, &fqdn);
    models::json::ApiDevice {
        id: device.id,
        fqdn: fqdn.clone(),
        name: device.name.clone(),
        dns_domain: device.dns_domain.clone(),
        snmp_community: device.snmp_community.clone(),
        base_mac: device.base_mac.clone(),
        polling_enabled: device.polling_enabled,
        os_info: device.os_info.clone(),
        device_type: device.device_type.clone(),
        software_version: device.software_version.clone(),
        up: up,
        seconds_since_last_poll: seconds_since_last_poll,
        interface_count: device.interfaces(connection).len() as u64,
    }
}

#[get("/devices")]
pub fn devices(mut connection: db::JaspyDB, imds: &State<Arc<Mutex<utilities::imds::IMDS>>>) -> Json<Vec<models::json::ApiDevice>> {
    let mut ret = Vec::new();
    for device in models::dbo::Device::all(&mut connection).iter() {
        ret.push(api_device(&mut connection, imds, device));
    }
    Json(ret)
}

#[get("/devices/<device_fqdn>")]
pub fn device_detail(mut connection: db::JaspyDB, device_fqdn: &str, imds: &State<Arc<Mutex<utilities::imds::IMDS>>>) -> Option<Json<models::json::ApiDeviceDetail>> {
    let device = models::dbo::Device::find_by_fqdn(&mut connection, &device_fqdn)?;

    // Live interface state (up/speed) from IMDS, keyed by ifIndex.
    let mut live: std::collections::HashMap<i32, (Option<bool>, Option<i32>)> = std::collections::HashMap::new();
    if let Ok(ref mut imds) = imds.inner().lock() {
        if let Some(device_metric) = imds.get_device(&device_fqdn) {
            for (ifindex, interface_metric) in device_metric.interfaces.iter() {
                let reported_speed = match interface_metric.speed_override {
                    Some(speed_override) => Some(speed_override),
                    None => interface_metric.speed,
                };
                live.insert(*ifindex, (interface_metric.up, reported_speed));
            }
        }
    }

    // Links are stored one-directionally (interfaces.connected_interface), and
    // discovery does not always resolve both ends. Union the reverse direction
    // — interfaces elsewhere pointing at this device — so the detail view
    // shows the link no matter which side discovery stored it on.
    let device_interfaces = device.interfaces(&mut connection);
    let interface_ids: Vec<i32> = device_interfaces.iter().map(|i| i.id).collect();
    let mut reverse_links: std::collections::HashMap<i32, models::json::ApiInterfaceConnection> = std::collections::HashMap::new();
    for remote in models::dbo::Interface::pointing_at(&mut connection, &interface_ids).iter() {
        let remote_device = remote.device(&mut connection);
        let connection_info = models::json::ApiInterfaceConnection {
            fqdn: format!("{}.{}", remote_device.name, remote_device.dns_domain),
            interface: remote.name(),
        };
        for target in [remote.connected_interface, remote.virtual_connection] {
            if let Some(target) = target {
                if interface_ids.contains(&target) {
                    reverse_links.entry(target).or_insert_with(|| connection_info.clone());
                }
            }
        }
    }

    let mut interfaces = Vec::new();
    for interface in device_interfaces.iter() {
        let connected_to = interface.peer_interface(&mut connection).map(|peer| {
            let peer_device = peer.device(&mut connection);
            models::json::ApiInterfaceConnection {
                fqdn: format!("{}.{}", peer_device.name, peer_device.dns_domain),
                interface: peer.name(),
            }
        }).or_else(|| reverse_links.get(&interface.id).cloned());
        let (up, speed) = live.get(&interface.index).cloned().unwrap_or((None, None));
        interfaces.push(models::json::ApiInterface {
            id: interface.id,
            index: interface.index,
            name: interface.name.clone(),
            display_name: interface.display_name.clone(),
            alias: interface.alias.clone(),
            description: interface.description.clone(),
            interface_type: interface.interface_type.clone(),
            polling_enabled: interface.polling_enabled,
            speed_override: interface.speed_override,
            connected_to: connected_to,
            up: up,
            speed: speed,
        });
    }
    interfaces.sort_by_key(|i| i.index);

    let device = api_device(&mut connection, imds, &device);
    Some(Json(models::json::ApiDeviceDetail { device: device, interfaces: interfaces }))
}

// Create/update/delete mirror the /dev/device handlers (device.rs) including
// their event semantics, so the UI API is self-contained for later auth.
#[post("/devices", data = "<device_json>")]
pub fn device_create(device_json: Json<models::dbo::NewDevice>, mut connection: db::JaspyDB, imds: &State<Arc<Mutex<utilities::imds::IMDS>>>, cache_controller: &State<Arc<Mutex<utilities::cache::CacheController>>>, msgbus: &State<Arc<Mutex<utilities::msgbus::MessageBus>>>) -> Option<Json<models::json::ApiDevice>> {
    if let Ok(created_device) = models::dbo::Device::create(&device_json, &mut connection) {
        let device_fqdn = format!("{}.{}", created_device.name, created_device.dns_domain);
        if let Ok(ref cache_controller) = cache_controller.lock() { cache_controller.invalidate_weathermap_cache(); }
        let event = models::events::Event::device_created_event(&device_fqdn);
        if let Ok(ref mut msgbus) = msgbus.lock() {
            msgbus.event(event);
        }
        return Some(Json(api_device(&mut connection, imds, &created_device)));
    }
    None
}

#[put("/devices/<device_fqdn>", data = "<device_json>")]
pub fn device_update(device_fqdn: &str, device_json: Json<models::dbo::NewDevice>, mut connection: db::JaspyDB, imds: &State<Arc<Mutex<utilities::imds::IMDS>>>, msgbus: &State<Arc<Mutex<utilities::msgbus::MessageBus>>>) -> Option<Json<models::json::ApiDevice>> {
    let mut device = models::dbo::Device::find_by_fqdn(&mut connection, &device_fqdn)?;

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
        // This MUST NOT raise an event!
        changed = true;
        device.snmp_community = device_json.snmp_community.clone();
    }
    if changed {
        if let Err(_) = device.update(&mut connection) {
            return None;
        }
    }
    Some(Json(api_device(&mut connection, imds, &device)))
}

#[delete("/devices/<device_fqdn>")]
pub fn device_delete(mut connection: db::JaspyDB, device_fqdn: &str, cache_controller: &State<Arc<Mutex<utilities::cache::CacheController>>>, msgbus: &State<Arc<Mutex<utilities::msgbus::MessageBus>>>) -> Option<Json<models::dbo::Device>> {
    let old_device = models::dbo::Device::find_by_fqdn(&mut connection, &device_fqdn)?;
    if let Err(e) = old_device.delete(&mut connection) {
        println!("[api] failed to delete {}: {}", device_fqdn, e);
        return None;
    }
    if let Ok(ref cache_controller) = cache_controller.lock() { cache_controller.invalidate_weathermap_cache(); }
    let event = models::events::Event::device_deleted_event(&device_fqdn);
    if let Ok(ref mut msgbus) = msgbus.lock() {
        msgbus.event(event);
    }
    Some(Json(old_device))
}

#[get("/clientlocations")]
pub fn clientlocations(mut connection: db::JaspyDB) -> Json<Vec<models::dbo::ClientLocation>> {
    Json(models::dbo::ClientLocation::all(&mut connection))
}

#[get("/event")]
pub fn event_get(mut connection: db::JaspyDB) -> Json<models::json::ApiEvent> {
    Json(models::json::ApiEvent { name: event_name(&mut connection) })
}

#[put("/event", data = "<event_json>")]
pub fn event_put(event_json: Json<models::json::ApiEvent>, mut connection: db::JaspyDB) -> Result<Json<models::json::ApiEvent>, (rocket::http::Status, Json<models::json::ApiError>)> {
    let event = event_json.into_inner();
    let json = serde_json::to_string(&event).map_err(|e| (rocket::http::Status::InternalServerError, Json(models::json::ApiError {
        error: format!("failed to serialize event: {}", e),
    })))?;
    if let Err(e) = models::dbo::Setting::set(&mut connection, EVENT_SETTING, &json) {
        println!("[api] failed to persist event: {}", e);
        return Err((rocket::http::Status::InternalServerError, Json(models::json::ApiError {
            error: format!("failed to persist event to database: {} (are the migrations up to date?)", e),
        })));
    }
    Ok(Json(event))
}

// Reset between events: delete every device (cascades interfaces, client
// locations and weathermap positions — same semantics as
// `jaspy-reset --cleanup-devices`) and clear the event name. Discovery config
// and other settings are kept.
#[post("/reset")]
pub fn reset(mut connection: db::JaspyDB, cache_controller: &State<Arc<Mutex<utilities::cache::CacheController>>>, msgbus: &State<Arc<Mutex<utilities::msgbus::MessageBus>>>) -> Json<models::json::ApiResetResult> {
    let mut devices_deleted = 0;
    for device in models::dbo::Device::all(&mut connection).iter() {
        let device_fqdn = format!("{}.{}", device.name, device.dns_domain);
        match device.delete(&mut connection) {
            Ok(_) => {
                devices_deleted += 1;
                let event = models::events::Event::device_deleted_event(&device_fqdn);
                if let Ok(ref mut msgbus) = msgbus.lock() {
                    msgbus.event(event);
                }
            },
            Err(e) => {
                println!("[api] reset: failed to delete {}: {}", device_fqdn, e);
            }
        }
    }
    let _ = models::dbo::Setting::delete(&mut connection, EVENT_SETTING);
    if let Ok(ref cache_controller) = cache_controller.lock() { cache_controller.invalidate_weathermap_cache(); }
    Json(models::json::ApiResetResult { devices_deleted: devices_deleted })
}

// Effective feature configuration + live connection state, for the
// Maintenance page's system status panel.
#[get("/system")]
pub fn system_status(
    system: &State<models::internal::SystemInfo>,
    runtime_info: &State<Arc<Mutex<models::internal::RuntimeInfo>>>,
    msgbus: &State<Arc<Mutex<utilities::msgbus::MessageBus>>>,
    discovery_control: &State<Arc<Mutex<crate::collectors::discovery::DiscoveryControl>>>,
) -> Json<models::json::ApiSystemStatus> {
    let startup_time = runtime_info.inner().lock().map(|r| r.startup_time).unwrap_or(0.0);
    let (mqtt_broker, mqtt_connected) = match msgbus.inner().lock() {
        Ok(msgbus) => (msgbus.broker(), msgbus.connection_status()),
        Err(_) => (None, None),
    };
    // Discovery scheduling is runtime-mutable (PUT /discovery/config), so read
    // the live control state rather than the startup config.
    let (discovery_periodic_enabled, discovery_interval_secs) = match discovery_control.inner().lock() {
        Ok(control) => (control.config.periodic_enabled, control.config.interval_secs),
        Err(_) => (false, 0),
    };
    let system = system.inner();
    Json(models::json::ApiSystemStatus {
        version: env!("CARGO_PKG_VERSION").to_string(),
        startup_time: startup_time,
        snmpbot_url: system.snmpbot_url.clone(),
        db_url: system.db_url.clone(),
        poller_enabled: system.poller_enabled,
        poll_loop_msecs: system.poll_loop_msecs,
        pinger_enabled: system.pinger_enabled,
        device_status_source: if system.pinger_enabled { "pinger".to_string() } else { "poller".to_string() },
        entitypoller_enabled: system.entitypoller_enabled,
        entitypoller_interval_msecs: system.entitypoller_interval_msecs,
        entitypoller_sensors_enabled: system.entitypoller_sensors_enabled,
        entitypoller_stp_enabled: system.entitypoller_stp_enabled,
        mqtt_enabled: mqtt_broker.is_some(),
        mqtt_broker: mqtt_broker,
        mqtt_connected: mqtt_connected,
        discovery_periodic_enabled: discovery_periodic_enabled,
        discovery_interval_secs: discovery_interval_secs,
        weathermap_dir: system.weathermap_dir.clone(),
    })
}

// Live update stream over WebSocket. The generic transport for pushing updates
// from the backend to the client: the server replays the topic's backlog on
// connect, then streams frames as they are published to utilities::livelog.
// Topics: "discovery" (run log lines, {"ts":..,"line":".."}) and
// "device:<fqdn>" (msgbus events for that device, models/events.rs JSON,
// live-only — no backlog).
#[get("/ws/logs/<topic>")]
pub fn ws_logs(ws: rocket_ws::WebSocket, topic: &str) -> rocket_ws::Channel<'static> {
    let topic = topic.to_string();
    ws.channel(move |mut stream| Box::pin(async move {
        use rocket::futures::{SinkExt, StreamExt};
        use rocket::tokio::sync::broadcast::error::RecvError;

        let (backlog, mut receiver) = utilities::livelog::subscribe(&topic);
        for frame in backlog.into_iter() {
            if stream.send(rocket_ws::Message::Text(frame)).await.is_err() {
                return Ok(());
            }
        }
        // Keepalive pings: intermediaries drop idle websockets, and a peer
        // that vanished without a FIN is only noticed by writing to it.
        let mut keepalive = rocket::tokio::time::interval(std::time::Duration::from_secs(30));
        keepalive.tick().await; // first tick is immediate; skip it
        loop {
            rocket::tokio::select! {
                _ = keepalive.tick() => {
                    if stream.send(rocket_ws::Message::Ping(Vec::new())).await.is_err() {
                        break;
                    }
                },
                frame = receiver.recv() => {
                    match frame {
                        Ok(frame) => {
                            if stream.send(rocket_ws::Message::Text(frame)).await.is_err() {
                                break;
                            }
                        },
                        // Consumer fell behind the broadcast buffer: skip the
                        // dropped lines and keep tailing.
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => break,
                    }
                },
                // We never act on client messages; polling the read side is
                // how we notice the peer went away (None/Err = closed).
                incoming = stream.next() => {
                    match incoming {
                        Some(Ok(_)) => {},
                        _ => break,
                    }
                }
            }
        }
        Ok(())
    }))
}
