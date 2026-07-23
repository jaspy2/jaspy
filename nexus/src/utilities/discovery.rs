// Discovery ingest: device/interface upsert and link reconciliation, shared by
// the HTTP PUT endpoints (routes/dev/discovery.rs) and the in-process discovery
// engine (collectors/discovery.rs). Lifted from the route handlers when the
// standalone Python `discover` tool was folded into nexus.
use crate::models;
use crate::utilities;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use crate::db::AnyConnection;

// Ingest problems go to stdout AND the "discovery" live-log topic, like the
// engine's own dlog! lines, so the web UI log shows why data went missing.
fn dlog(line: String) {
    println!("{}", line);
    utilities::livelog::publish("discovery", &line);
}

pub fn ingest_device(
    connection: &mut AnyConnection,
    msgbus: &Arc<Mutex<utilities::msgbus::MessageBus>>,
    cache_controller: &Arc<Mutex<utilities::cache::CacheController>>,
    discovered_device: &models::json::DiscoveredDevice,
) {
    let discovered_device_interfaces: &HashMap<String, models::json::DiscoveredInterface> = &discovered_device.interfaces;
    let device_fqdn = format!("{}.{}", discovered_device.name, discovered_device.dns_domain);

    let device: models::dbo::Device;
    let existing_device = models::dbo::Device::find_by_hostname_and_domain_name(connection, &discovered_device.name, &discovered_device.dns_domain);
    match existing_device {
        Some(mut existing_device) => {
            // Emit change events like routes/dev/device.rs::update does; the
            // snmp_community change MUST NOT raise an event.
            if existing_device.os_info != discovered_device.os_info {
                let event = models::events::Event::device_os_info_changed_event(&device_fqdn, &existing_device.os_info, &discovered_device.os_info);
                if let Ok(ref mut msgbus) = msgbus.lock() {
                    msgbus.event(event);
                }
            }
            if existing_device.base_mac != discovered_device.base_mac {
                let event = models::events::Event::device_base_mac_changed_event(&device_fqdn, &existing_device.base_mac, &discovered_device.base_mac);
                if let Ok(ref mut msgbus) = msgbus.lock() {
                    msgbus.event(event);
                }
            }

            existing_device.base_mac = discovered_device.base_mac.clone();
            existing_device.os_info = discovered_device.os_info.clone();
            existing_device.snmp_community = discovered_device.snmp_community.clone();
            existing_device.software_version = discovered_device.software_version.clone();
            existing_device.device_type = discovered_device.device_type.clone();

            match existing_device.update(connection) {
                Ok(_) => {
                    device = existing_device;
                },
                Err(e) => {
                    dlog(format!("[discovery] [{}] failed to update device in db: {}", device_fqdn, e));
                    return;
                }
            }
        },
        None => {
            let new_device = models::dbo::NewDevice {
                name: discovered_device.name.clone(),
                dns_domain: discovered_device.dns_domain.clone(),
                snmp_community: discovered_device.snmp_community.clone(),
                base_mac: discovered_device.base_mac.clone(),
                os_info: discovered_device.os_info.clone(),
                polling_enabled: None,
                software_version: discovered_device.software_version.clone(),
                device_type: discovered_device.device_type.clone(),
            };

            let event = models::events::Event::device_created_event(&device_fqdn);
            if let Ok(ref mut msgbus) = msgbus.lock() {
                msgbus.event(event);
            }

            match models::dbo::Device::create(&new_device, connection) {
                Ok(created_device) => {
                    device = created_device;
                },
                Err(e) => {
                    dlog(format!("[discovery] [{}] failed to create device in db: {}", device_fqdn, e));
                    return;
                }
            }
        }
    }

    let current_interfaces: Vec<models::dbo::Interface> = device.interfaces(connection);
    let mut found_interface_names: HashSet<String> = HashSet::new();
    for (_key, interface) in discovered_device_interfaces.iter() {
        found_interface_names.insert(interface.name.clone());
        let mut selected_interface: Option<&models::dbo::Interface> = None;
        for current_interface in current_interfaces.iter() {
            if interface.name == current_interface.name {
                selected_interface = Some(current_interface);
                break;
            }
        }
        match selected_interface {
            Some(selected_interface) => {
                let mut updated_interface: models::dbo::Interface = (*selected_interface).clone();
                updated_interface.index = interface.index;
                updated_interface.interface_type = interface.interface_type.clone();
                updated_interface.name = interface.name.clone();
                updated_interface.alias = interface.alias.clone();
                updated_interface.description = interface.description.clone();
                // Only overwrite media when this crawl actually classified it,
                // so a transient ENTITY-MIB failure doesn't wipe a prior value.
                if interface.media.is_some() {
                    updated_interface.media = interface.media.clone();
                }
                // Same for the CDP neighbor: only refresh it when this crawl
                // saw one, so a transient cdpCacheTable failure keeps the last
                // known neighbor rather than blanking the column.
                if interface.cdp_device_id.is_some() {
                    updated_interface.cdp_device_id = interface.cdp_device_id.clone();
                    updated_interface.cdp_device_port = interface.cdp_device_port.clone();
                }
                match updated_interface.update(connection) {
                    Ok(_) => {},
                    Err(e) => {
                        // Nonfatal: the rest of the interfaces still ingest.
                        dlog(format!("[discovery] [{}] failed to update interface {} in db: {}", device_fqdn, interface.name, e));
                    }
                }
            },
            None => {
                let new_interface = models::dbo::NewInterface {
                    alias: interface.alias.clone(),
                    name: interface.name.clone(),
                    description: interface.description.clone(),
                    device_id: device.id,
                    index: interface.index,
                    interface_type: interface.interface_type.clone(),
                    media: interface.media.clone(),
                    cdp_device_id: interface.cdp_device_id.clone(),
                    cdp_device_port: interface.cdp_device_port.clone(),
                };

                match models::dbo::Interface::create(&new_interface, connection) {
                    Ok(_) => {},
                    Err(e) => {
                        // Nonfatal: the rest of the interfaces still ingest.
                        dlog(format!("[discovery] [{}] failed to create interface {} in db: {}", device_fqdn, interface.name, e));
                    }
                }
            }
        }
    }

    for current_interface in current_interfaces.iter() {
        if !found_interface_names.contains(&current_interface.name) {
            match current_interface.delete(connection) {
                Ok(_) => {},
                Err(e) => {
                    // Nonfatal: a stale interface row lingers until next run.
                    dlog(format!("[discovery] [{}] failed to delete stale interface {} from db: {}", device_fqdn, current_interface.name, e));
                }
            }
        }
    }

    // Devices/interfaces are part of the weathermap topology too.
    if let Ok(ref cache_controller) = cache_controller.lock() {
        cache_controller.invalidate_weathermap_cache();
    }
}

// TODO: this might be better placed in dbo logic?
fn clear_connection(interface: &models::dbo::Interface, connection: &mut AnyConnection) {
    if interface.connected_interface.is_none() { return; }

    let mut new_local_interface: models::dbo::Interface = interface.clone();
    new_local_interface.connected_interface = None;
    match new_local_interface.update(connection) {
        Ok(_) => {},
        Err(e) => {
            dlog(format!("[discovery] failed to clear link from interface {} in db: {}", interface.name, e));
        }
    }
}

pub fn ingest_links(
    connection: &mut AnyConnection,
    cache_controller: &Arc<Mutex<utilities::cache::CacheController>>,
    links: &models::json::LinkInfo,
) {
    let link_infos: &HashMap<String, Option<models::json::LinkPeerInfo>> = &links.interfaces;
    let fqdn_splitted: Vec<&str> = links.device_fqdn.splitn(2, ".").collect();
    if fqdn_splitted.len() != 2 {
        dlog(format!("[discovery] ignoring links for {}: fqdn has no domain part", links.device_fqdn));
        return;
    }

    let local_device: models::dbo::Device;
    let local_device_result = models::dbo::Device::find_by_hostname_and_domain_name(connection, &fqdn_splitted[0].to_string(), &fqdn_splitted[1].to_string());
    match local_device_result {
        Some(local_device_result) => {
            local_device = local_device_result;
        },
        None => {
            dlog(format!("[discovery] ignoring links for {}: device not found in db", links.device_fqdn));
            return;
        }
    }

    for local_interface in local_device.interfaces(connection).iter() {
        let peer_interface_info: &models::json::LinkPeerInfo;
        match link_infos.get(&local_interface.name) {
            Some(peer_interface_info_opt) => {
                match peer_interface_info_opt {
                    Some(some_peer_interface_info) => {
                        peer_interface_info = some_peer_interface_info;
                    },
                    None => {
                        // TBD, should we clear peer connection? This must respect stability.
                        if !links.topology_stable { clear_connection(local_interface, connection); }
                        continue;
                    }
                }
            },
            None => {
                // TBD, should we clear peer connection? This must respect stability.
                if !links.topology_stable { clear_connection(local_interface, connection); }
                continue;
            }
        }

        let peer_device: models::dbo::Device;
        match models::dbo::Device::find_by_hostname_and_domain_name(connection, &peer_interface_info.name, &peer_interface_info.dns_domain) {
            Some(some_peer_device) => {
                peer_device = some_peer_device;
            },
            None => {
                match local_interface.connected_interface {
                    Some(_) => {
                        if !links.topology_stable { clear_connection(local_interface, connection); }
                        continue;
                    },
                    None => {
                        continue;
                    }
                }
            }
        }

        // todo if peer interface is same noop, if different then change, if no peer interface then change
        match local_interface.peer_interface(connection) {
            Some(peer_interface) => {
                let mut create_link = false;
                let mut clear_other = false;
                if peer_interface.device_id != peer_device.id {
                    // Device changed
                    create_link = true;
                    clear_other = true;
                } else if peer_interface_info.interface != peer_interface.name {
                    // Interface in device changed
                    create_link = true;
                    clear_other = true;
                }
                if create_link {
                    match peer_device.interface_by_name(connection, &peer_interface_info.interface) {
                        Some(new_peer_interface) => {
                            // TBD: create link other way too? maybe not?
                            let mut new_local_interface: models::dbo::Interface = local_interface.clone();
                            new_local_interface.connected_interface = Some(new_peer_interface.id);
                            match new_local_interface.update(connection) {
                                Ok(_) => {},
                                Err(e) => {
                                    dlog(format!("[discovery] [{}] failed to store link for interface {} in db: {}", links.device_fqdn, local_interface.name, e));
                                }
                            }
                        },
                        None => {
                            // other side interface not found, do some guesswork and/or clear any possible link?
                        }
                    }
                }
                if clear_other && !links.topology_stable {
                    let mut new_peer_interface: models::dbo::Interface = peer_interface.clone();
                    new_peer_interface.connected_interface = None;
                    match new_peer_interface.update(connection) {
                        Ok(_) => {},
                        Err(e) => {
                            dlog(format!("[discovery] [{}] failed to clear stale peer link from interface {} in db: {}", links.device_fqdn, peer_interface.name, e));
                        }
                    }
                }
            },
            None => {
                match peer_device.interface_by_name(connection, &peer_interface_info.interface) {
                    Some(new_peer_interface) => {
                        // TBD: create link other way too? maybe not?
                        let mut new_local_interface: models::dbo::Interface = local_interface.clone();
                        new_local_interface.connected_interface = Some(new_peer_interface.id);
                        match new_local_interface.update(connection) {
                            Ok(_) => {},
                            Err(_) => {
                                // TODO: log?
                            }
                        }
                    },
                    None => {
                        // other side interface not found, do some guesswork and/or clear any possible link?
                    }
                }
            }
        }
    }

    // Invalidate weathermap topology cache
    if let Ok(ref cache_controller) = cache_controller.lock() {
        cache_controller.invalidate_weathermap_cache();
    }
}
