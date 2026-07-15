// Seeds the parts of the mock network that don't flow from the discovery
// crawl: client locations (DHCP option-82 data normally pushed by external
// integrations) and the event name shown in the UI header.
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

    // Wait for the discovery crawl to ingest devices, then insert each client
    // once. Bounded wait: ~5 minutes.
    let mut pending = seed_clients();
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
        if pending.is_empty() {
            println!("[mock] seeded client locations and event name");
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
    fn seed_client_ips_and_macs_are_unique() {
        let clients = seed_clients();
        let ips: std::collections::HashSet<&str> = clients.iter().map(|c| c.ip).collect();
        let macs: std::collections::HashSet<&str> = clients.iter().map(|c| c.mac).collect();
        assert_eq!(ips.len(), clients.len());
        assert_eq!(macs.len(), clients.len());
    }
}
