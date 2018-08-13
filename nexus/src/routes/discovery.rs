extern crate rocket_contrib;
use models;
use db;
use std::collections::{HashSet,HashMap};

#[put("/device", data = "<discovery_json>")]
fn discovery_device(discovery_json: rocket_contrib::Json<models::json::DiscoveryInfo>, connection: db::Connection) {
    let discovered_device : &models::json::DiscoveredDevice = &discovery_json.device_info;
    let discovered_device_interfaces : &Vec<models::json::DiscoveredInterface> = &discovery_json.interface_info;

    let device : models::dbo::Device;
    let existing_device = models::dbo::Device::find_by_fqdn(&connection, &discovered_device.name, &discovered_device.dns_domain);
    match existing_device {
        Some(mut existing_device) => {
            existing_device.base_mac = discovered_device.base_mac.clone();
            existing_device.os_info = discovered_device.os_info.clone();
            existing_device.snmp_community = discovered_device.snmp_community.clone();
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
            };
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
    for interface in discovered_device_interfaces.iter() {
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
}

#[put("/links", data = "<links_json>")]
fn discovery_links(links_json: rocket_contrib::Json<models::json::LinkInfo>, connection: db::Connection) {
    let link_infos : &HashMap<String, Option<models::json::LinkPeerInfo>> = &links_json.interfaces;
    let fqdn_splitted : Vec<&str> = links_json.device_fqdn.splitn(2, ".").collect();
    if fqdn_splitted.len() != 2 {
        // TODO: log?
        return;
    }

    let local_device : models::dbo::Device;
    let local_device_result = models::dbo::Device::find_by_fqdn(&connection, &fqdn_splitted[0].to_string(), &fqdn_splitted[1].to_string());
    match local_device_result {
        Some(local_device_result) => {
            local_device = local_device_result;
        },
        None => {
            // TODO: log?
            return;
        }
    }

    // TODO: immediately resolve peer device, deduplicates some code

    for local_interface in local_device.interfaces(&connection).iter() {
        match link_infos.get(&local_interface.name) {
            Some(peer_interface_info_opt) => {
                match peer_interface_info_opt {
                    Some(peer_interface_info) => {
                        match local_interface.peer_interface(&connection) {
                            Some(peer_interface) => {
                                match models::dbo::Device::find_by_fqdn(&connection, &peer_interface_info.name, &peer_interface_info.dns_domain) {
                                    Some(peer_device) => {
                                        let mut create_link : bool = false;
                                        let mut clear_other : bool = false;
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
                                        if clear_other {
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
                                        // wtf, peer device is not found, clear the link if unstable?
                                    }
                                }
                            },
                            None => {
                                match models::dbo::Device::find_by_fqdn(&connection, &peer_interface_info.name, &peer_interface_info.dns_domain) {
                                    Some(peer_device) => {
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
                                    },
                                    None => {
                                        // wtf, peer device is not found, clear the link if unstable?
                                    }
                                }
                            }
                        }
                    },
                    None => {
                        if !links_json.topology_stable {
                            match local_interface.connected_interface {
                                Some(_connected_interface) => {
                                    // TBD: should this clear peer connection too? probably not (will be cleared when discovered)
                                    let mut new_local_interface : models::dbo::Interface = local_interface.clone();
                                    new_local_interface.connected_interface = None;
                                    match new_local_interface.update(&connection) {
                                        Ok(_) => {},
                                        Err(_) => {
                                            // TODO: log?
                                        }
                                    }
                                },
                                None => {
                                    // no-op
                                }
                            }
                        }
                    }
                }
            },
            None => {
                match local_interface.connected_interface {
                    Some(_connected_interface) => {
                        // TBD: should this clear peer connection too? probably not (will be cleared when discovered)
                        let mut new_local_interface : models::dbo::Interface = local_interface.clone();
                        new_local_interface.connected_interface = None;
                        match new_local_interface.update(&connection) {
                            Ok(_) => {},
                            Err(_) => {
                                // TODO: log?
                            }
                        }
                    },
                    None => {
                        // no-op
                    }
                }
            }
        }
    }
}