extern crate rocket_contrib;
use models;
use db;
use std::sync::{Arc,Mutex};
use std::collections::{HashSet,HashMap};
use rocket::State;
use utilities;

// TODO: GH#9 Move everything to v1 API
#[put("/device", data = "<discovery_json>")]
pub fn discovery_device(discovery_json: rocket_contrib::json::Json<models::json::DiscoveredDevice>, connection: db::Connection, metric_miss_cache: State<Arc<Mutex<models::metrics::DeviceMetricRefreshCacheMiss>>>, msgbus: State<Arc<Mutex<utilities::msgbus::MessageBus>>>) {
    let discovered_device : &models::json::DiscoveredDevice = &discovery_json.into_inner();
    let discovered_device_interfaces : &HashMap<String, models::json::DiscoveredInterface> = &discovered_device.interfaces;

    let device : models::dbo::Device;
    let existing_device = models::dbo::Device::find_by_hostname_and_domain_name(&connection, &discovered_device.name, &discovered_device.dns_domain);
    match existing_device {
        Some(mut existing_device) => {
            // TODO: attr compare, event if change except for snmp com
            existing_device.base_mac = discovered_device.base_mac.clone();
            existing_device.os_info = discovered_device.os_info.clone();
            existing_device.snmp_community = discovered_device.snmp_community.clone();
            existing_device.software_version = discovered_device.software_version.clone();
            existing_device.device_type = discovered_device.device_type.clone();

            match existing_device.update(&connection) {
                Ok(_) => {
                    // TODO: Log update?
                    device = existing_device;
                },
                Err(_) => {
                    // TODO: sane logging / return
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

            let device_fqdn = format!("{}.{}", new_device.name, new_device.dns_domain);
            let event = models::events::Event::device_created_event(&device_fqdn);
            if let Ok(ref mut msgbus) = msgbus.lock() {
                msgbus.event(event);
            }

            match models::dbo::Device::create(&new_device, &connection) {
                Ok(created_device) => {
                    device = created_device;
                },
                Err(_) => {
                    // TODO: sane logging / return
                    return;
                }
            }
        }
    }
    
    let current_interfaces : Vec<models::dbo::Interface> = device.interfaces(&connection);
    let mut found_interface_names : HashSet<String> = HashSet::new();
    for (_key, interface) in discovered_device_interfaces.iter() {
        found_interface_names.insert(interface.name.clone());
        let mut selected_interface : Option<&models::dbo::Interface> = None;
        for current_interface in current_interfaces.iter() {
            if interface.name == current_interface.name {
                selected_interface = Some(current_interface);
                break;
            }
        }
        match selected_interface {
            Some(selected_interface) => {
                let mut updated_interface : models::dbo::Interface = (*selected_interface).clone();
                updated_interface.index = interface.index;
                updated_interface.interface_type = interface.interface_type.clone();
                updated_interface.name = interface.name.clone();
                updated_interface.alias = interface.alias.clone();
                updated_interface.description = interface.description.clone();
                match updated_interface.update(&connection) {
                    Ok(_) => {},
                    Err(_) => {
                        // TODO: logging, nonfatal
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
                };
                
                match models::dbo::Interface::create(&new_interface, &connection) {
                    Ok(_) => {
                    },
                    Err(_) => {
                        // Todo, logging? :) this is nonfatal
                    }
                }
            }
        }
    }

    for current_interface in current_interfaces.iter() {
        if !found_interface_names.contains(&current_interface.name) {
            match current_interface.delete(&connection) {
                Ok(_) => {},
                Err(_) => {
                    // Todo, logging? nonfatal
                }
            }
        }
    }

    // TODO: optimize: only invalidate metric miss cache if stuff changes
    let device_fqdn = format!("{}.{}", device.name, device.dns_domain);
    if let Ok(ref mut metric_miss_cache) = metric_miss_cache.inner().lock() {
        if !metric_miss_cache.miss_set.contains(&device_fqdn) { metric_miss_cache.miss_set.insert(device_fqdn); }
    }
}

// TODO: this might be better placed in an utility module or maybe in dbo logic?
fn clear_connection(interface: &models::dbo::Interface, connection: &db::Connection) {
    if interface.connected_interface.is_none() { return; }

    let mut new_local_interface : models::dbo::Interface = interface.clone();
    new_local_interface.connected_interface = None;
    match new_local_interface.update(&connection) {
        Ok(_) => {},
        Err(_) => {
            // TODO: log?
        }
    }
}

#[put("/links", data = "<links_json>")]
pub fn discovery_links(
    links_json: rocket_contrib::json::Json<models::json::LinkInfo>,
    connection: db::Connection,
    metric_miss_cache: State<Arc<Mutex<models::metrics::DeviceMetricRefreshCacheMiss>>>,
    cache_controller: State<Arc<Mutex<utilities::cache::CacheController>>>
) {
    let link_infos : &HashMap<String, Option<models::json::LinkPeerInfo>> = &links_json.interfaces;
    let fqdn_splitted : Vec<&str> = links_json.device_fqdn.splitn(2, ".").collect();
    if fqdn_splitted.len() != 2 {
        // TODO: log?
        return;
    }

    let local_device : models::dbo::Device;
    let local_device_result = models::dbo::Device::find_by_hostname_and_domain_name(&connection, &fqdn_splitted[0].to_string(), &fqdn_splitted[1].to_string());
    match local_device_result {
        Some(local_device_result) => {
            local_device = local_device_result;
        },
        None => {
            // TODO: log?
            return;
        }
    }

    for local_interface in local_device.interfaces(&connection).iter() {
        let peer_interface_info : &models::json::LinkPeerInfo;
        match link_infos.get(&local_interface.name) {
            Some(peer_interface_info_opt) => {
                match peer_interface_info_opt {
                    Some(some_peer_interface_info) => {
                        peer_interface_info = some_peer_interface_info;
                    },
                    None => {
                        // TBD, should we clear peer connection? This must respect stability.
                        if !links_json.topology_stable { clear_connection(local_interface, &connection); }
                        continue;
                    }
                }
            },
            None => {
                // TBD, should we clear peer connection? This must respect stability.
                if !links_json.topology_stable { clear_connection(local_interface, &connection); }
                continue;
            }
        }

        let peer_device : models::dbo::Device;
        match models::dbo::Device::find_by_hostname_and_domain_name(&connection, &peer_interface_info.name, &peer_interface_info.dns_domain) {
            Some(some_peer_device) => {
                peer_device = some_peer_device;
            },
            None => {
                match local_interface.connected_interface {
                    Some(_) => {
                        if !links_json.topology_stable { clear_connection(local_interface, &connection); }
                        continue;
                    },
                    None => {
                        continue;
                    }
                }
            }
        }

        // todo if peer interface is same noop, if different then change, if no peer interface then change
        match local_interface.peer_interface(&connection) {
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
                    match peer_device.interface_by_name(&connection, &peer_interface_info.interface) {
                        Some(new_peer_interface) => {
                            // TBD: create link other way too? maybe not?
                            let mut new_local_interface : models::dbo::Interface = local_interface.clone();
                            new_local_interface.connected_interface = Some(new_peer_interface.id);
                            match new_local_interface.update(&connection) {
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
                if clear_other && !links_json.topology_stable {
                    let mut new_peer_interface : models::dbo::Interface = peer_interface.clone();
                    new_peer_interface.connected_interface = None;
                    match new_peer_interface.update(&connection) {
                        Ok(_) => {},
                        Err(_) => {
                            // TODO: log?
                        }
                    }
                }
            },
            None => {
                match peer_device.interface_by_name(&connection, &peer_interface_info.interface) {
                    Some(new_peer_interface) => {
                        // TBD: create link other way too? maybe not?
                        let mut new_local_interface : models::dbo::Interface = local_interface.clone();
                        new_local_interface.connected_interface = Some(new_peer_interface.id);
                        match new_local_interface.update(&connection) {
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

    // TODO: optimize: only invalidate metric miss cache if stuff changes
    let device_fqdn = format!("{}.{}", local_device.name, local_device.dns_domain);
    if let Ok(ref mut metric_miss_cache) = metric_miss_cache.inner().lock() {
        if !metric_miss_cache.miss_set.contains(&device_fqdn) { metric_miss_cache.miss_set.insert(device_fqdn); }
    }

    // Invalidate weathermap topology cache
    if let Ok(ref cache_controller) = cache_controller.lock() { cache_controller.invalidate_weathermap_cache(); }
}
