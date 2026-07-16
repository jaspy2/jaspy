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

// The addresses a device fqdn resolves to — nothing in the DB stores IPs;
// every collector (snmpbot, pinger) dials by name, so resolution IS the
// address jaspy talks to. Empty when the name does not resolve.
//
// Cached for a short TTL: the device page refetches every 10s, and blocking
// getaddrinfo can stall for the full resolver timeout on unresolvable names
// — so negative results are deliberately cached too.
const IP_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

struct IpCache {
    entries: Mutex<std::collections::HashMap<String, (std::time::Instant, Vec<String>)>>,
}

impl IpCache {
    fn new() -> IpCache {
        IpCache { entries: Mutex::new(std::collections::HashMap::new()) }
    }

    fn get(&self, fqdn: &str, now: std::time::Instant) -> Option<Vec<String>> {
        match self.entries.lock() {
            Ok(entries) => entries
                .get(fqdn)
                .filter(|(resolved, _)| now.duration_since(*resolved) < IP_CACHE_TTL)
                .map(|(_, ips)| ips.clone()),
            Err(_) => None,
        }
    }

    fn put(&self, fqdn: &str, ips: Vec<String>, now: std::time::Instant) {
        if let Ok(mut entries) = self.entries.lock() {
            // Deleted devices' entries age out instead of accumulating.
            entries.retain(|_, (resolved, _)| now.duration_since(*resolved) < IP_CACHE_TTL);
            entries.insert(fqdn.to_string(), (now, ips));
        }
    }
}

fn resolve_device_ips(fqdn: &str) -> Vec<String> {
    static CACHE: std::sync::OnceLock<IpCache> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(IpCache::new);
    let now = std::time::Instant::now();
    if let Some(ips) = cache.get(fqdn, now) {
        return ips;
    }
    let ips = resolve_ips_uncached(fqdn);
    cache.put(fqdn, ips.clone(), now);
    ips
}

// Mock mode installs a static fqdn -> addresses map at startup (the fake
// devices have no DNS); anything not in the map resolves normally.
static IP_OVERRIDES: std::sync::OnceLock<std::collections::HashMap<String, Vec<String>>> = std::sync::OnceLock::new();

pub fn install_ip_overrides(overrides: std::collections::HashMap<String, Vec<String>>) {
    let _ = IP_OVERRIDES.set(overrides);
}

fn resolve_ips_uncached(fqdn: &str) -> Vec<String> {
    use std::net::ToSocketAddrs;
    if let Some(ips) = IP_OVERRIDES.get().and_then(|overrides| overrides.get(fqdn)) {
        return ips.clone();
    }
    match (fqdn, 0u16).to_socket_addrs() {
        Ok(addrs) => order_device_ips(addrs.map(|a| a.ip())),
        Err(_) => Vec::new(),
    }
}

// v4 before v6, deduplicated, resolver order otherwise preserved.
fn order_device_ips(addrs: impl Iterator<Item = std::net::IpAddr>) -> Vec<String> {
    let mut v4: Vec<String> = Vec::new();
    let mut v6: Vec<String> = Vec::new();
    for ip in addrs {
        let rendered = ip.to_string();
        let bucket = if ip.is_ipv4() { &mut v4 } else { &mut v6 };
        if !bucket.contains(&rendered) {
            bucket.push(rendered);
        }
    }
    v4.extend(v6);
    v4
}

fn event_name(connection: &mut db::AnyConnection) -> Option<String> {
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

fn api_device(connection: &mut db::AnyConnection, imds: &State<Arc<Mutex<utilities::imds::IMDS>>>, device: &models::dbo::Device) -> models::json::ApiDevice {
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
pub fn device_detail(mut connection: db::JaspyDB, device_fqdn: &str, imds: &State<Arc<Mutex<utilities::imds::IMDS>>>, vlan_store: &State<Arc<Mutex<crate::collectors::vlanpoller::VlanStore>>>, lag_store: &State<Arc<Mutex<crate::collectors::lagpoller::LagStore>>>) -> Option<Json<models::json::ApiDeviceDetail>> {
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

    // VLAN membership from the in-memory vlanpoller store, keyed by ifIndex.
    let device_vlans = match vlan_store.inner().lock() {
        Ok(store) => store.device_vlans(&device_fqdn),
        Err(_) => crate::collectors::vlanpoller::DeviceVlans::default(),
    };
    let vlans = &device_vlans.interfaces;

    // Port-channel membership from the in-memory lagpoller store.
    let device_lags = match lag_store.inner().lock() {
        Ok(store) => store.device_lags(&device_fqdn).unwrap_or_default(),
        Err(_) => crate::collectors::lagpoller::DeviceLags::default(),
    };
    // member ifIndex -> aggregate ifIndex, for the per-interface chip.
    let mut member_groups: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    for (agg, group) in device_lags.groups.iter() {
        for member in group.members.keys() {
            member_groups.insert(*member, *agg);
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
        let interface_vlans = vlans.get(&(interface.index as i64));
        let port_channel = member_groups
            .get(&(interface.index as i64))
            .map(|agg| device_interfaces.iter().find(|i| i.index as i64 == *agg).map(|i| i.name.clone()).unwrap_or_else(|| format!("ifIndex {}", agg)));
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
            native_vlan: interface_vlans.and_then(|v| v.native_vlan),
            tagged_vlans: interface_vlans.map(|v| v.tagged_vlans.clone()),
            port_channel: port_channel,
        });
    }
    interfaces.sort_by_key(|i| i.index);

    // Port-channels: LAG data joined with the interface rows built above,
    // plus the mismatch warnings (LACP state + topology + monitored far end).
    let mut port_channels: Vec<models::json::ApiPortChannel> = Vec::new();
    for (agg, group) in device_lags.groups.iter() {
        let agg_interface = interfaces.iter().find(|i| i.index as i64 == *agg);
        let meta: std::collections::HashMap<i64, crate::collectors::lagpoller::MemberMeta> = group
            .members
            .keys()
            .filter_map(|member| {
                interfaces.iter().find(|i| i.index as i64 == *member).map(|i| {
                    (*member, crate::collectors::lagpoller::MemberMeta {
                        name: i.name.clone(),
                        connected_to_fqdn: i.connected_to.as_ref().map(|c| c.fqdn.clone()),
                    })
                })
            })
            .collect();
        // LAG data of the monitored peer devices the members are wired to,
        // for the far-end cross-check.
        let mut peer_lags: std::collections::HashMap<String, crate::collectors::lagpoller::DeviceLags> = std::collections::HashMap::new();
        if let Ok(store) = lag_store.inner().lock() {
            for peer_fqdn in meta.values().filter_map(|m| m.connected_to_fqdn.clone()) {
                if let Some(peer) = store.device_lags(&peer_fqdn) {
                    peer_lags.insert(peer_fqdn, peer);
                }
            }
        }
        let warnings = crate::collectors::lagpoller::port_channel_warnings(group, &meta, &peer_lags);
        let members = group.members.iter().map(|(member, state)| {
            let member_interface = interfaces.iter().find(|i| i.index as i64 == *member);
            models::json::ApiPortChannelMember {
                ifindex: *member,
                name: member_interface.map(|i| i.name.clone()),
                up: member_interface.and_then(|i| i.up),
                connected_to: member_interface.and_then(|i| i.connected_to.clone()),
                actor_state: state.actor_state.clone(),
                partner_state: state.partner_state.clone(),
                partner_port: state.partner_port,
                bundled: crate::collectors::lagpoller::lacp_bundled(&state.actor_state),
            }
        }).collect();
        port_channels.push(models::json::ApiPortChannel {
            ifindex: *agg,
            name: agg_interface.map(|i| i.name.clone()),
            up: agg_interface.and_then(|i| i.up),
            protocol: group.protocol.clone(),
            partner_system_id: group.partner_system_id.clone(),
            members: members,
            warnings: warnings,
        });
    }

    // The device's VLAN id -> name catalog: every named VLAN, plus any id
    // referenced by an interface that has no name row (name stays null).
    let mut vlan_ids: std::collections::BTreeSet<i64> = device_vlans.names.keys().cloned().collect();
    for interface_vlans in device_vlans.interfaces.values() {
        vlan_ids.extend(interface_vlans.native_vlan.iter());
        vlan_ids.extend(interface_vlans.tagged_vlans.iter());
    }
    let vlans: Vec<models::json::ApiVlan> = vlan_ids.into_iter().map(|id| models::json::ApiVlan {
        id: id,
        name: device_vlans.names.get(&id).cloned(),
    }).collect();

    let device = api_device(&mut connection, imds, &device);
    Some(Json(models::json::ApiDeviceDetail {
        device: device,
        interfaces: interfaces,
        vlans: vlans,
        port_channels: port_channels,
        ip_addresses: resolve_device_ips(&device_fqdn),
    }))
}

// Latest entitypoller results (entity sensors + per-VLAN STP) for one device,
// straight from the in-memory store — no DB. Unknown fqdn and not-yet-polled
// devices both answer 200 with empty arrays; the UI treats them the same.
#[get("/devices/<device_fqdn>/entity")]
pub fn device_entity(device_fqdn: &str, entity_metrics: &State<Arc<Mutex<crate::collectors::entitypoller::EntityMetricsStore>>>) -> Json<models::json::ApiDeviceEntity> {
    let entity = match entity_metrics.inner().lock() {
        Ok(store) => store.device_entity(device_fqdn),
        Err(_) => models::json::ApiDeviceEntity { sensors: Vec::new(), stp: Vec::new(), stp_bridges: Vec::new() },
    };
    Json(entity)
}

fn device_base_macs(connection: &mut db::AnyConnection) -> std::collections::HashMap<String, Option<String>> {
    models::dbo::Device::all(connection)
        .into_iter()
        .map(|device| (format!("{}.{}", device.name, device.dns_domain), device.base_mac))
        .collect()
}

// Which VLANs have STP data, for the STP page's selector. Root resolution is
// cheap: the device with STP ports on the vlan but no root-role port; when
// that is ambiguous, fall back to matching the devices' reported root bridge
// MAC against discovery's base_mac records.
#[get("/stp")]
pub fn stp_summary(
    mut connection: db::JaspyDB,
    entity_metrics: &State<Arc<Mutex<crate::collectors::entitypoller::EntityMetricsStore>>>,
) -> Json<Vec<models::json::ApiStpVlanSummary>> {
    use crate::utilities::stp::normalize_mac;

    let (ports, bridges) = match entity_metrics.inner().lock() {
        Ok(store) => store.network_stp(),
        Err(_) => Default::default(),
    };
    let base_macs = device_base_macs(&mut connection);
    let fqdn_by_mac: std::collections::HashMap<String, String> = base_macs
        .iter()
        .filter_map(|(fqdn, mac)| mac.as_ref().map(|m| (normalize_mac(m), fqdn.clone())))
        .collect();

    let vlans: std::collections::BTreeSet<i64> = ports.values().flatten().map(|p| p.vlan).collect();
    let summaries = vlans.into_iter().map(|vlan| {
        let mut node_count = 0;
        let mut blocked_port_count = 0;
        let mut root_candidates: Vec<&String> = Vec::new();
        for (fqdn, device_ports) in ports.iter() {
            let on_vlan: Vec<_> = device_ports.iter().filter(|p| p.vlan == vlan).collect();
            if on_vlan.is_empty() {
                continue;
            }
            node_count += 1;
            blocked_port_count += on_vlan.iter().filter(|p| p.role == "alternate" || p.role == "backUp").count() as i64;
            if !on_vlan.iter().any(|p| p.role == "root") {
                root_candidates.push(fqdn);
            }
        }
        let root_fqdn = match root_candidates.as_slice() {
            [single] => Some((*single).clone()),
            _ => {
                // Majority vote over the reported root MACs, mapped to a
                // monitored device when possible.
                let mut votes: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
                for bridge in bridges.values().flatten().filter(|b| b.vlan == vlan) {
                    if let Some(mac) = bridge.root_mac.as_ref() {
                        *votes.entry(normalize_mac(mac)).or_default() += 1;
                    }
                }
                votes.into_iter().max_by_key(|(_, count)| *count).and_then(|(mac, _)| fqdn_by_mac.get(&mac).cloned())
            }
        };
        let vlan_bridges: Vec<_> = bridges.values().flatten().filter(|b| b.vlan == vlan).collect();
        models::json::ApiStpVlanSummary {
            vlan: vlan,
            root_fqdn: root_fqdn,
            node_count: node_count,
            blocked_port_count: blocked_port_count,
            topology_changes: vlan_bridges.iter().filter_map(|b| b.topology_changes).max(),
            time_since_topology_change_secs: vlan_bridges.iter().filter_map(|b| b.time_since_topology_change_secs).min(),
        }
    }).collect();
    Json(summaries)
}

// The computed active spanning tree for one VLAN: entitypoller STP data
// joined with the DB link topology (see utilities/stp.rs).
#[get("/stp/<vlan>")]
pub fn stp_tree(
    mut connection: db::JaspyDB,
    vlan: i64,
    entity_metrics: &State<Arc<Mutex<crate::collectors::entitypoller::EntityMetricsStore>>>,
    cache_controller: &State<Arc<Mutex<utilities::cache::CacheController>>>,
    lag_store: &State<Arc<Mutex<crate::collectors::lagpoller::LagStore>>>,
) -> Json<models::json::ApiStpTree> {
    let (ports, bridges) = match entity_metrics.inner().lock() {
        Ok(store) => store.network_stp(),
        Err(_) => Default::default(),
    };
    let topology = crate::routes::dev::weathermap::cached_topology_data(&mut connection, cache_controller.inner());
    let base_macs = device_base_macs(&mut connection);
    // Aggregate STP ports (port-channels) resolve adjacency via their members.
    let lag_members = lag_store.inner().lock().map(|store| store.lag_members()).unwrap_or_default();
    let inputs = crate::utilities::stp::StpInputs {
        ports: &ports,
        bridges: &bridges,
        base_macs: &base_macs,
        topology: &topology,
        lag_members: &lag_members,
    };
    Json(crate::utilities::stp::build_stp_tree(&inputs, vlan))
}

// Network-wide VLAN inventory (id, per-device names, port usage), straight
// from the in-memory vlanpoller store — no DB. Empty until the first poll.
#[get("/vlans")]
pub fn vlans(vlan_store: &State<Arc<Mutex<crate::collectors::vlanpoller::VlanStore>>>) -> Json<Vec<models::json::ApiVlanSummary>> {
    let vlans = match vlan_store.inner().lock() {
        Ok(store) => store.network_vlans(),
        Err(_) => Vec::new(),
    };
    Json(vlans)
}

// Queue an immediate VLAN membership poll for one device. The vlanpoller
// supervisor drains the queue on its next 1s tick, so fresh data lands in
// GET /devices/<fqdn> within a couple of seconds instead of the regular
// (multi-minute) interval. 202 = queued; 409 = already queued/running or the
// vlanpoller is disabled; 404 = unknown device or no SNMP community.
#[post("/devices/<device_fqdn>/vlans/poll")]
pub fn device_vlan_poll(
    mut connection: db::JaspyDB,
    device_fqdn: &str,
    system: &State<models::internal::SystemInfo>,
    control: &State<Arc<Mutex<crate::collectors::vlanpoller::VlanPollerControl>>>,
) -> Result<rocket::http::Status, (rocket::http::Status, Json<models::json::ApiError>)> {
    let not_found = |error: String| (rocket::http::Status::NotFound, Json(models::json::ApiError { error }));
    let device = models::dbo::Device::find_by_fqdn(&mut connection, &device_fqdn)
        .ok_or_else(|| not_found(format!("device not found: {}", device_fqdn)))?;
    if device.snmp_community.is_none() {
        return Err(not_found(format!("device has no SNMP community: {}", device_fqdn)));
    }
    if !system.vlanpoller_enabled {
        return Err((rocket::http::Status::Conflict, Json(models::json::ApiError {
            error: "the vlanpoller is disabled (JASPY_ENABLE_VLANPOLLER)".to_string(),
        })));
    }
    match control.inner().lock() {
        Ok(mut control) => {
            if !control.pending.insert(device_fqdn.to_string()) {
                return Err((rocket::http::Status::Conflict, Json(models::json::ApiError {
                    error: "a VLAN poll for this device is already in progress".to_string(),
                })));
            }
            Ok(rocket::http::Status::Accepted)
        }
        Err(_) => Err((rocket::http::Status::InternalServerError, Json(models::json::ApiError {
            error: "internal error: vlanpoller control unavailable".to_string(),
        }))),
    }
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
    pool: &State<db::Pool>,
) -> Json<models::json::ApiSystemStatus> {
    let startup_time = runtime_info.inner().lock().map(|r| r.startup_time).unwrap_or(0.0);
    // Live database probe. get_timeout, never get(): the blocking variant can
    // stall the handler for the pool's full 30s checkout timeout when the
    // database is down.
    let (db_connected, db_migrations_pending) =
        match pool.get_timeout(std::time::Duration::from_millis(500)) {
            Ok(mut conn) => (true, db::has_pending_migrations(&mut *conn).ok()),
            Err(_) => (false, None),
        };
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
        db_backend: system.db_backend.clone(),
        db_connected: db_connected,
        db_migrations_pending: db_migrations_pending,
        poller_enabled: system.poller_enabled,
        poll_loop_msecs: system.poll_loop_msecs,
        pinger_enabled: system.pinger_enabled,
        device_status_source: if system.pinger_enabled { "pinger".to_string() } else { "poller".to_string() },
        entitypoller_enabled: system.entitypoller_enabled,
        entitypoller_interval_msecs: system.entitypoller_interval_msecs,
        entitypoller_sensors_enabled: system.entitypoller_sensors_enabled,
        entitypoller_stp_enabled: system.entitypoller_stp_enabled,
        vlanpoller_enabled: system.vlanpoller_enabled,
        vlanpoller_interval_msecs: system.vlanpoller_interval_msecs,
        lagpoller_enabled: system.lagpoller_enabled,
        lagpoller_interval_msecs: system.lagpoller_interval_msecs,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    #[test]
    fn device_ips_dedupe_and_order_v4_first() {
        let addrs: Vec<IpAddr> = vec![
            "::1".parse().unwrap(),
            "10.0.0.1".parse().unwrap(),
            "10.0.0.1".parse().unwrap(),
            "fe80::1".parse().unwrap(),
            "10.0.0.2".parse().unwrap(),
        ];
        assert_eq!(order_device_ips(addrs.into_iter()), vec!["10.0.0.1", "10.0.0.2", "::1", "fe80::1"]);
        assert!(order_device_ips(std::iter::empty()).is_empty());
    }

    #[test]
    fn resolve_localhost_and_unresolvable() {
        // The uncached path: unit tests must not touch the process-global cache.
        let ips = resolve_ips_uncached("localhost");
        assert!(ips.iter().any(|ip| ip == "127.0.0.1"), "localhost should resolve v4: {:?}", ips);
        // RFC 6761 reserves .invalid: guaranteed NXDOMAIN.
        assert!(resolve_ips_uncached("no-such-device.invalid").is_empty());
    }

    #[test]
    fn ip_cache_serves_within_ttl_and_expires_after() {
        let cache = IpCache::new();
        let t0 = std::time::Instant::now();
        cache.put("sw1.x", vec!["10.0.0.1".to_string()], t0);
        assert_eq!(cache.get("sw1.x", t0).as_deref(), Some(&["10.0.0.1".to_string()][..]));
        assert_eq!(cache.get("sw1.x", t0 + IP_CACHE_TTL / 2).as_deref(), Some(&["10.0.0.1".to_string()][..]));
        assert_eq!(cache.get("sw1.x", t0 + IP_CACHE_TTL), None, "expired at TTL");
        assert_eq!(cache.get("other.x", t0), None);
    }

    #[test]
    fn ip_cache_caches_negative_results_and_prunes_on_put() {
        let cache = IpCache::new();
        let t0 = std::time::Instant::now();
        // A failed resolution (empty vec) is a cached answer, not a miss.
        cache.put("ghost.x", Vec::new(), t0);
        assert_eq!(cache.get("ghost.x", t0), Some(Vec::new()));

        // Inserting after the TTL prunes the stale entry.
        cache.put("sw1.x", vec!["10.0.0.1".to_string()], t0 + IP_CACHE_TTL * 2);
        assert!(cache.entries.lock().unwrap().get("ghost.x").is_none(), "stale entries pruned");
    }
}
