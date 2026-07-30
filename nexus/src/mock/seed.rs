// Seeds the parts of the mock network that don't flow from the discovery
// crawl: client locations (DHCP option-82 data normally pushed by external
// integrations), the event name shown in the UI header, and weathermap
// positions (normally dragged into place by hand) so the map renders laid
// out on first load.
//
// Devices and links land in the DB via the real discovery engine crawling the
// fake snmpbot, so this thread first waits for the crawl to ingest the access
// switches (matched by base MAC, which discovery derives from the mock
// dot1dBaseBridgeAddress), then inserts deterministic client rows.
use crate::mock::topology;
use crate::models;

struct SeedClient {
    ip: &'static str,
    mac: &'static str,
    device: &'static str, // bare mock device name
    port_info: &'static str,
}

// A VLAN root pre-marked as an expected/known separate tree, so the per-root
// STP acknowledgement is demoable out of the box (see StpExpectedRoot).
struct SeedExpectedRoot {
    vlan: i64,
    device: &'static str, // bare mock device name
    note: &'static str,
}

// dist1 self-roots VLAN 30 against core1 (a directly-linked split brain — see
// topology.rs). Acknowledging it leaves core1 as the single legitimate root, so
// /stp?vlan=30 shows dist1 badged "expected" and the "STP: multiple roots" issue
// stays cleared until the ack is removed (which brings the issue back — the
// interactive half of the demo).
fn seed_expected_roots() -> Vec<SeedExpectedRoot> {
    vec![SeedExpectedRoot {
        vlan: 30,
        device: "dist1",
        note: "dist1 self-roots by design (leftover) — known split, not in prod",
    }]
}

// Hand-laid weathermap layout: firewall on top, then core, dist, access rows.
// Only seeded where no position exists yet (a persisted JASPY_DB_URL keeps
// whatever the developer dragged).
fn seed_positions() -> Vec<(&'static str, f64, f64)> {
    vec![
        ("fw1", 400.0, 60.0),
        ("core1", 400.0, 180.0),
        ("dist1", 250.0, 300.0),
        ("dist2", 550.0, 300.0),
        ("access-hall-a-01", 120.0, 430.0),
        ("access-hall-a-02", 320.0, 430.0),
        ("access-hall-b-01", 520.0, 430.0),
        ("wlc1", 700.0, 430.0),
    ]
}

fn seed_clients() -> Vec<SeedClient> {
    vec![
        SeedClient { ip: "10.66.1.10", mac: "02:aa:00:00:01:0a", device: "access-hall-a-01", port_info: "1/1" },
        SeedClient { ip: "10.66.1.11", mac: "02:aa:00:00:01:0b", device: "access-hall-a-01", port_info: "1/2" },
        SeedClient { ip: "10.66.1.12", mac: "02:aa:00:00:01:0c", device: "access-hall-a-01", port_info: "1/4" },
        SeedClient { ip: "10.66.1.13", mac: "02:aa:00:00:01:0d", device: "access-hall-a-01", port_info: "1/5" },
        SeedClient { ip: "10.66.2.10", mac: "02:aa:00:00:02:0a", device: "access-hall-a-02", port_info: "1/1" },
        SeedClient { ip: "10.66.2.11", mac: "02:aa:00:00:02:0b", device: "access-hall-a-02", port_info: "1/3" },
        SeedClient { ip: "10.66.2.12", mac: "02:aa:00:00:02:0c", device: "access-hall-a-02", port_info: "1/7" },
        SeedClient { ip: "10.66.3.10", mac: "02:aa:00:00:03:0a", device: "access-hall-b-01", port_info: "1/1" },
        SeedClient { ip: "10.66.3.11", mac: "02:aa:00:00:03:0b", device: "access-hall-b-01", port_info: "1/2" },
        SeedClient { ip: "10.66.3.12", mac: "02:aa:00:00:03:0c", device: "access-hall-b-01", port_info: "1/8" },
    ]
}

pub fn spawn(db_url: String) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || run(&db_url))
}

fn run(db_url: &str) {
    let topo = topology::build();
    let device_mac = |name: &str| -> Option<String> {
        topo.devices.iter().position(|d| d.name == name).map(topology::base_mac)
    };

    let mut connection = match connect_with_retry(db_url, 30) {
        Some(c) => c,
        None => {
            println!("[mock] seeder could not connect to the database; skipping client seed");
            return;
        }
    };

    // Event name for the UI header; mirrors PUT /api/v1/event.
    let event_json = serde_json::json!({"name": "Mock Event"}).to_string();
    let _ = models::dbo::Setting::set(&mut connection, "event", &event_json);

    // Expected-root acknowledgements don't depend on the discovery crawl (they
    // key on VLAN + fqdn), so seed them upfront. Only when absent, so a removal
    // via the UI sticks across restarts on a persisted JASPY_DB_URL.
    let existing: std::collections::HashSet<(i64, String)> = models::dbo::StpExpectedRoot::all(&mut connection)
        .into_iter()
        .map(|r| (r.vlan, r.root_fqdn))
        .collect();
    for seed in seed_expected_roots() {
        let root_fqdn = format!("{}.{}", seed.device, topology::DOMAIN);
        if existing.contains(&(seed.vlan, root_fqdn.clone())) {
            continue;
        }
        let expected = models::dbo::StpExpectedRoot {
            vlan: seed.vlan,
            root_fqdn,
            root_mac: None,
            acked_at: crate::utilities::tools::get_time_msecs() as i64,
            acked_by: Some("mock".to_string()),
            note: Some(seed.note.to_string()),
        };
        let _ = expected.upsert(&mut connection);
    }

    // Wait for the discovery crawl to ingest devices, then insert each client
    // and device position once. Bounded wait: ~5 minutes.
    let mut pending = seed_clients();
    let mut pending_positions = seed_positions();
    for _ in 0..300 {
        pending.retain(|client| {
            let mac = match device_mac(client.device) {
                Some(mac) => mac,
                None => return false, // topology mismatch: drop, tested against build()
            };
            let device = match models::dbo::Device::by_base_mac(&mac, &mut connection) {
                Some(device) => device,
                None => return true, // not ingested yet, retry
            };
            if models::dbo::ClientLocation::by_ip(&client.ip.to_string(), &mut connection).is_none() {
                let _ = models::dbo::ClientLocation::create(
                    &models::dbo::NewClientLocation {
                        device_id: device.id,
                        ip_address: client.ip.to_string(),
                        hw_address: client.mac.to_string(),
                        port_info: client.port_info.to_string(),
                    },
                    &mut connection,
                );
            }
            false
        });
        pending_positions.retain(|(name, x, y)| {
            let mac = match device_mac(name) {
                Some(mac) => mac,
                None => return false, // topology mismatch: drop, tested against build()
            };
            let device = match models::dbo::Device::by_base_mac(&mac, &mut connection) {
                Some(device) => device,
                None => return true, // not ingested yet, retry
            };
            if device.weathermap_info(&mut connection).is_none() {
                let _ = models::dbo::WeathermapDeviceInfo::create(
                    &models::dbo::NewWeathermapDeviceInfo {
                        x: *x,
                        y: *y,
                        super_node: false,
                        expanded_by_default: true,
                        device_id: device.id,
                    },
                    &mut connection,
                );
            }
            false
        });
        if pending.is_empty() && pending_positions.is_empty() {
            println!("[mock] seeded client locations, weathermap positions and event name");
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    println!("[mock] seeder timed out waiting for discovery to ingest devices");
}

fn connect_with_retry(db_url: &str, attempts: u32) -> Option<crate::db::AnyConnection> {
    for _ in 0..attempts {
        if let Ok(connection) = crate::db::establish(db_url) {
            return Some(connection);
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_clients_reference_existing_access_switches() {
        let topo = topology::build();
        for client in seed_clients() {
            assert!(
                topo.devices.iter().any(|d| d.name == client.device),
                "seed client {} references unknown device {}",
                client.ip,
                client.device
            );
        }
    }

    #[test]
    fn seed_positions_cover_every_device_exactly_once() {
        let topo = topology::build();
        let positions = seed_positions();
        let names: std::collections::HashSet<&str> = positions.iter().map(|(name, _, _)| *name).collect();
        assert_eq!(names.len(), positions.len(), "duplicate position seed");
        for dev in topo.devices.iter() {
            assert!(names.contains(dev.name), "device {} has no seeded position", dev.name);
        }
        assert_eq!(positions.len(), topo.devices.len(), "position seed references unknown devices");
    }

    #[test]
    fn seed_expected_roots_reference_stp_running_devices() {
        let topo = topology::build();
        for seed in seed_expected_roots() {
            let dev = topo.devices.iter().find(|d| d.name == seed.device)
                .unwrap_or_else(|| panic!("expected-root seed references unknown device {}", seed.device));
            assert!(
                dev.stp_vlans.contains(&seed.vlan),
                "device {} does not run STP on VLAN {} (expected-root seed is stale)",
                seed.device, seed.vlan,
            );
        }
    }

    #[test]
    fn seed_client_ips_and_macs_are_unique() {
        let clients = seed_clients();
        let ips: std::collections::HashSet<&str> = clients.iter().map(|c| c.ip).collect();
        let macs: std::collections::HashSet<&str> = clients.iter().map(|c| c.mac).collect();
        assert_eq!(ips.len(), clients.len());
        assert_eq!(macs.len(), clients.len());
    }
}
