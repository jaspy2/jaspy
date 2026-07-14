// End-to-end tests: boot the real jaspy-nexus binary against a mock snmpbot and
// an ephemeral Postgres, then assert on SNMP queries issued, rows written to
// Postgres, and Prometheus metrics exposed.
mod common;

use std::time::{Duration, Instant};

use diesel::prelude::*;
use serde_json::json;

use common::*;

const FQDN: &str = "sw1.test.example";
const COMMUNITY: &str = "testcomm";

fn device_body(polling: bool) -> serde_json::Value {
    json!({
        "name": "sw1", "dnsDomain": "test.example",
        "snmpCommunity": COMMUNITY, "pollingEnabled": polling
    })
}

/// A DiscoveredDevice payload with the two interfaces our fixtures describe.
fn discovery_body(name: &str, domain: &str) -> serde_json::Value {
    json!({
        "name": name, "dnsDomain": domain, "snmpCommunity": COMMUNITY,
        "baseMac": null, "osInfo": null, "deviceType": null, "softwareVersion": null,
        "interfaces": {
            "GigabitEthernet0/1": {"index":10101,"interfaceType":"ethernetCsmacd","displayName":null,"name":"GigabitEthernet0/1","alias":null,"description":"GigabitEthernet0/1"},
            "GigabitEthernet0/2": {"index":10102,"interfaceType":"ethernetCsmacd","displayName":null,"name":"GigabitEthernet0/2","alias":null,"description":"GigabitEthernet0/2"}
        }
    })
}

/// Find a metric line `name{...labels...} VALUE TS` where every string in
/// `labels` is a substring of the label set, and return VALUE.
fn metric_value(body: &str, name: &str, labels: &[&str]) -> Option<i64> {
    let prefix = format!("{}{{", name);
    for line in body.lines() {
        if !line.starts_with(&prefix) {
            continue;
        }
        if labels.iter().all(|l| line.contains(l)) {
            let close = line.rfind('}')?;
            let rest = line[close + 1..].trim();
            let val = rest.split_whitespace().next()?;
            return val.parse::<i64>().ok();
        }
    }
    None
}

fn wait_until<F: Fn() -> bool>(timeout: Duration, f: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

// ---------------------------------------------------------------------------
// 1. Poller issues the right snmpbot queries and renders interface metrics
// ---------------------------------------------------------------------------
#[test]
fn poller_queries_and_interface_metrics() {
    let pg = PgHarness::start();
    let mock = SnmpbotMock::start();
    let iftable = mock.stub_table(FQDN, COMMUNITY, "IF-MIB::ifTable", &read_fixture("iftable.json"));
    let ifxtable = mock.stub_table(FQDN, COMMUNITY, "IF-MIB::ifXTable", &read_fixture("ifxtable.json"));
    let other = mock.stub_other_tables();

    let nexus = Nexus::builder(&pg.db_url)
        .snmpbot(&mock.url())
        .poller(true)
        .poll_loop_msecs(300)
        .start();

    nexus.post_json("/dev/device", &device_body(true));
    nexus.put_json("/dev/discovery/device", &discovery_body("sw1", "test.example"));

    let slow = nexus.wait_for_metric("jaspy_interface_octets", Duration::from_secs(15));

    // (a) correct SNMP queries, and nothing extra
    assert!(iftable.hits() >= 1, "IF-MIB::ifTable should have been queried");
    assert!(ifxtable.hits() >= 1, "IF-MIB::ifXTable should have been queried");
    assert_eq!(other.hits(), 0, "no snmpbot tables other than ifTable/ifXTable should be queried");

    // (b) correct interface metrics (values straight from the fixtures)
    assert_eq!(
        metric_value(&slow, "jaspy_interface_octets", &["name=\"GigabitEthernet0/1\"", "direction=\"rx\""]),
        Some(1_000_000)
    );
    assert_eq!(
        metric_value(&slow, "jaspy_interface_octets", &["name=\"GigabitEthernet0/1\"", "direction=\"tx\""]),
        Some(2_000_000)
    );
    assert_eq!(
        metric_value(&slow, "jaspy_interface_speed", &["name=\"GigabitEthernet0/1\""]),
        Some(1000)
    );

    let fast = nexus.metrics_fast();
    assert_eq!(
        metric_value(&fast, "jaspy_interface_up", &["name=\"GigabitEthernet0/1\""]),
        Some(1),
        "Gi0/1 is ifOperStatus=up"
    );
    assert_eq!(
        metric_value(&fast, "jaspy_interface_up", &["name=\"GigabitEthernet0/2\""]),
        Some(0),
        "Gi0/2 is ifOperStatus=down"
    );
}

// ---------------------------------------------------------------------------
// 1b. Entitypoller renders entity sensor + per-VLAN STP metrics
// ---------------------------------------------------------------------------
#[test]
fn entitypoller_sensor_and_stp_metrics() {
    let pg = PgHarness::start();
    let mock = SnmpbotMock::start();

    // entitypoller addresses snmpbot hosts inline as community@fqdn, and
    // per-VLAN queries as community@vlan@fqdn.
    let host = format!("{}@{}", COMMUNITY, FQDN);
    let vlan_host = format!("{}@100@{}", COMMUNITY, FQDN);

    let phys = mock.stub_host_table(&host, "ENTITY-MIB::entPhysicalTable", &read_fixture("entphysicaltable.json"));
    mock.stub_host_table(&host, "ENTITY-SENSOR-MIB::entPhySensorTable", &read_fixture("entphysensortable.json"));
    mock.stub_host_table(&host, "CISCO-ENTITY-SENSOR-MIB::entSensorValueTable", &read_fixture("entsensorvaluetable.json"));
    let role = mock.stub_host_table(&host, "CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleTable", &read_fixture("stpxrstpportroletable.json"));
    mock.stub_host_table(&host, "IF-MIB::ifTable", &read_fixture("stp_iftable.json"));
    mock.stub_host_table(&vlan_host, "BRIDGE-MIB::dot1dBasePortTable", &read_fixture("dot1dbaseporttable.json"));
    let stp = mock.stub_host_table(&vlan_host, "BRIDGE-MIB::dot1dStpPortTable", &read_fixture("dot1dstpporttable.json"));

    let nexus = Nexus::builder(&pg.db_url)
        .snmpbot(&mock.url())
        .entitypoller(true)
        .entitypoller_interval_msecs(300)
        .start();

    nexus.post_json("/dev/device", &device_body(true));
    nexus.put_json("/dev/discovery/device", &discovery_body("sw1", "test.example"));

    let body = nexus.wait_for_metric("jaspy_stp_port_state", Duration::from_secs(20));

    assert!(phys.hits() >= 1, "entPhysicalTable should have been queried");
    assert!(role.hits() >= 1, "stpxRSTPPortRoleTable should have been queried");
    assert!(stp.hits() >= 1, "dot1dStpPortTable should have been queried");

    // (a) entity sensor: 45000 milli-celsius -> 45, associated with Gi0/1 via
    //     the sensor name's leading token.
    assert_eq!(
        metric_value(&body, "jaspy_sensors", &[
            "sensor_name=\"GigabitEthernet0/1 Module Temperature Sensor\"",
            "value_type=\"celsius\"",
            "interface_name=\"GigabitEthernet0/1\"",
        ]),
        Some(45)
    );

    // (b) STP per-VLAN port metrics: vlan 100, bridge port 5, resolved to real
    //     ifIndex 10101 / GigabitEthernet0/1 via dot1dBasePort + ifTable.
    let stp_labels = [
        "fqdn=\"sw1.test.example\"",
        "vlan=\"100\"",
        "stp_port_id=\"5\"",
        "interface_id=\"10101\"",
        "interface_name=\"GigabitEthernet0/1\"",
    ];
    // The cisco sensor fixture uses snmpbot's real empty-table shape
    // ("Entries": null, Go nil slice) — it must parse cleanly, not error.
    assert!(
        !nexus.log().contains("error parsing json"),
        "snmpbot null-Entries responses must decode; log:\n{}",
        nexus.log()
    );

    assert_eq!(metric_value(&body, "jaspy_stp_port_state", &stp_labels), Some(5), "forwarding");
    assert_eq!(metric_value(&body, "jaspy_stp_port_role", &stp_labels), Some(3), "designated");
    assert_eq!(metric_value(&body, "jaspy_stp_port_enabled", &stp_labels), Some(1), "enabled");
    assert_eq!(metric_value(&body, "jaspy_stp_port_designated_cost", &stp_labels), Some(4));
    assert_eq!(metric_value(&body, "jaspy_stp_port_path_cost", &stp_labels), Some(19));
    assert_eq!(metric_value(&body, "jaspy_stp_port_priority", &stp_labels), Some(128));
    assert_eq!(metric_value(&body, "jaspy_stp_port_forward_transitions", &stp_labels), Some(2));
}

// ---------------------------------------------------------------------------
// 1c. In-process discovery engine: crawl, device metadata, links, periodic
// ---------------------------------------------------------------------------

fn snmp_table_body(id: &str, entries: Vec<serde_json::Value>) -> String {
    json!({"ID": id, "IndexKeys": [], "ObjectKeys": [], "Entries": entries}).to_string()
}

/// Stub every snmpbot table/object the discovery engine reads for one device
/// with two ports (Gi0/1 uplink to `peer`, Gi0/2 unconnected).
fn stub_discovery_device<'a>(
    mock: &'a SnmpbotMock,
    fqdn: &str,
    mac_octet: &str,
    peer_bare_name: Option<&str>,
    cdp_peer_fqdn: Option<&str>,
) -> httpmock::Mock<'a> {
    let mac_colon = format!("aa:bb:cc:dd:ee:{}", mac_octet);
    let mac_space = format!("aa bb cc dd ee {}", mac_octet);

    mock.stub_object(fqdn, COMMUNITY, "SNMPv2-MIB::sysDescr", "Cisco IOS test software");
    mock.stub_object(fqdn, COMMUNITY, "BRIDGE-MIB::dot1dBaseBridgeAddress", &mac_space);
    mock.stub_object(fqdn, COMMUNITY, "LLDP-MIB::lldpLocChassisId", &mac_space);

    let ifxtable = mock.stub_table(fqdn, COMMUNITY, "IF-MIB::ifXTable", &snmp_table_body("IF-MIB::ifXTable", vec![
        json!({"HostID": fqdn, "Index": {"IF-MIB::ifIndex": 10101},
               "Objects": {"IF-MIB::ifName": "GigabitEthernet0/1", "IF-MIB::ifAlias": "uplink"}}),
        json!({"HostID": fqdn, "Index": {"IF-MIB::ifIndex": 10102},
               "Objects": {"IF-MIB::ifName": "GigabitEthernet0/2", "IF-MIB::ifAlias": ""}}),
    ]));
    mock.stub_table(fqdn, COMMUNITY, "IF-MIB::ifTable", &snmp_table_body("IF-MIB::ifTable", vec![
        json!({"HostID": fqdn, "Index": {"IF-MIB::ifIndex": 10101},
               "Objects": {"IF-MIB::ifDescr": "GigabitEthernet0/1", "IF-MIB::ifType": "ethernetCsmacd", "IF-MIB::ifPhysAddress": format!("aa:bb:cc:dd:01:{}", mac_octet)}}),
        json!({"HostID": fqdn, "Index": {"IF-MIB::ifIndex": 10102},
               "Objects": {"IF-MIB::ifDescr": "GigabitEthernet0/2", "IF-MIB::ifType": "ethernetCsmacd", "IF-MIB::ifPhysAddress": format!("aa:bb:cc:dd:02:{}", mac_octet)}}),
    ]));
    mock.stub_table(fqdn, COMMUNITY, "LLDP-MIB::lldpLocPortTable", &snmp_table_body("LLDP-MIB::lldpLocPortTable", vec![
        json!({"HostID": fqdn, "Index": {"LLDP-MIB::lldpLocPortNum": 1},
               "Objects": {"LLDP-MIB::lldpLocPortIdSubtype": "interfaceName", "LLDP-MIB::lldpLocPortId": "GigabitEthernet0/1", "LLDP-MIB::lldpLocPortDesc": "GigabitEthernet0/1"}}),
    ]));
    let rem_entries = match peer_bare_name {
        Some(peer) => vec![
            json!({"HostID": fqdn, "Index": {"LLDP-MIB::lldpRemLocalPortNum": 1},
                   "Objects": {"LLDP-MIB::lldpRemSysName": peer, "LLDP-MIB::lldpRemChassisId": "ff ff ff ff ff ff",
                               "LLDP-MIB::lldpRemPortId": "GigabitEthernet0/1", "LLDP-MIB::lldpRemPortIdSubtype": "interfaceName"}}),
        ],
        None => vec![],
    };
    mock.stub_table(fqdn, COMMUNITY, "LLDP-MIB::lldpRemTable", &snmp_table_body("LLDP-MIB::lldpRemTable", rem_entries));
    let cdp_entries = match cdp_peer_fqdn {
        Some(peer) => vec![
            json!({"HostID": fqdn, "Index": {"CISCO-CDP-MIB::cdpCacheIfIndex": 10101, "CISCO-CDP-MIB::cdpCacheDeviceIndex": 1},
                   "Objects": {"CISCO-CDP-MIB::cdpCacheDeviceId": peer, "CISCO-CDP-MIB::cdpCacheDevicePort": "GigabitEthernet0/1"}}),
        ],
        None => vec![],
    };
    mock.stub_table(fqdn, COMMUNITY, "CISCO-CDP-MIB::cdpCacheTable", &snmp_table_body("CISCO-CDP-MIB::cdpCacheTable", cdp_entries));
    mock.stub_table(fqdn, COMMUNITY, "ENTITY-MIB::entPhysicalTable", &snmp_table_body("ENTITY-MIB::entPhysicalTable", vec![
        json!({"HostID": fqdn, "Index": {"ENTITY-MIB::entPhysicalIndex": 1},
               "Objects": {"ENTITY-MIB::entPhysicalClass": "chassis", "ENTITY-MIB::entPhysicalDescr": "test chassis",
                           "ENTITY-MIB::entPhysicalModelName": "WS-C2960X-24", "ENTITY-MIB::entPhysicalSoftwareRev": "15.2(2)E"}}),
    ]));
    let _ = mac_colon;
    ifxtable
}

#[derive(QueryableByName)]
struct DiscDeviceRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    base_mac: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    os_info: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    device_type: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    software_version: Option<String>,
}

#[derive(QueryableByName)]
struct PeerRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    peer_device: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    peer_interface: String,
}

fn link_peer(conn: &mut diesel::pg::PgConnection, device: &str, interface: &str) -> Option<(String, String)> {
    let rows: Vec<PeerRow> = query_rows(conn, &format!(
        "select d2.name as peer_device, i2.name as peer_interface \
         from interfaces i \
         join devices d on d.id = i.device_id \
         join interfaces i2 on i2.id = i.connected_interface \
         join devices d2 on d2.id = i2.device_id \
         where d.name = '{}' and i.name = '{}'", device, interface));
    rows.into_iter().next().map(|r| (r.peer_device, r.peer_interface))
}

#[test]
fn discovery_engine_crawls_and_links() {
    let pg = PgHarness::start();
    let mock = SnmpbotMock::start();
    let broker = MqttBroker::start();

    // sw1 <-Gi0/1-> sw2 via LLDP (bare rem-sysnames, resolved through the
    // configured search domain); sw1 additionally announces sw2 via CDP.
    stub_discovery_device(&mock, "sw1.test.example", "01", Some("sw2"), Some("sw2.test.example"));
    stub_discovery_device(&mock, "sw2.test.example", "02", Some("sw1"), None);

    let nexus = Nexus::builder(&pg.db_url)
        .snmpbot(&mock.url())
        .mqtt(&broker.server())
        .start();

    let resp = nexus.post_json("/dev/discovery/run", &json!({
        "rootDevice": "sw1.test.example",
        "community": COMMUNITY,
        "dnsDomains": ["test.example"]
    }));
    assert_eq!(resp.status().as_u16(), 202, "trigger should be accepted");

    assert!(
        wait_until(Duration::from_secs(20), || {
            let status = nexus.get_json("/dev/discovery/status");
            status["running"] == json!(false) && !status["lastFinished"].is_null()
        }),
        "discovery run should finish; status: {:?}\nlog: {}",
        nexus.get_json("/dev/discovery/status"), nexus.log()
    );

    // Status DTO reflects the run.
    let status = nexus.get_json("/dev/discovery/status");
    assert_eq!(status["devicesFound"], json!(2), "status: {:?}", status);
    assert_eq!(status["linksFound"], json!(2), "status: {:?}", status);
    assert!(status["lastError"].is_null(), "status: {:?}", status);

    // Devices + metadata landed in Postgres.
    let mut conn = pg.conn();
    let devices: Vec<DiscDeviceRow> = query_rows(
        &mut conn,
        "select name, base_mac, os_info, device_type, software_version from devices order by name",
    );
    assert_eq!(devices.len(), 2);
    assert_eq!(devices[0].name, "sw1");
    assert_eq!(devices[0].base_mac.as_deref(), Some("aa:bb:cc:dd:ee:01"));
    assert_eq!(devices[0].os_info.as_deref(), Some("Cisco IOS test software"));
    assert_eq!(devices[0].device_type.as_deref(), Some("WS-C2960X-24"));
    assert_eq!(devices[0].software_version.as_deref(), Some("15.2(2)E"));
    assert_eq!(devices[1].name, "sw2");
    assert_eq!(devices[1].base_mac.as_deref(), Some("aa:bb:cc:dd:ee:02"));

    // Both local sides of the link recorded.
    assert_eq!(
        link_peer(&mut conn, "sw1", "GigabitEthernet0/1"),
        Some(("sw2".to_string(), "GigabitEthernet0/1".to_string()))
    );
    assert_eq!(
        link_peer(&mut conn, "sw2", "GigabitEthernet0/1"),
        Some(("sw1".to_string(), "GigabitEthernet0/1".to_string()))
    );

    // Weathermap reflects the topology.
    let wmap = nexus.get_json("/dev/weathermap/");
    let connected = &wmap["devices"]["sw1.test.example"]["interfaces"]["GigabitEthernet0/1"]["connectedTo"];
    assert_eq!(connected["fqdn"], "sw2.test.example");

    // deviceCreated MQTT events for both crawled devices.
    for fqdn in ["sw1.test.example", "sw2.test.example"] {
        let created = broker.wait_for_event(Duration::from_secs(5), |t, p| t == "jaspy/nexus/deviceCreated" && p.contains(fqdn));
        assert!(created.is_some(), "expected deviceCreated for {}; got {:?}", fqdn, broker.events());
    }
}

#[test]
fn discovery_periodic_runs_and_config_disable() {
    let pg = PgHarness::start();
    let mock = SnmpbotMock::start();

    // Single device with no neighbors; periodic every second.
    let ifxtable = stub_discovery_device(&mock, "sw1.test.example", "01", None, None);

    let nexus = Nexus::builder(&pg.db_url)
        .snmpbot(&mock.url())
        .env("JASPY_DISCOVERY_ROOT_DEVICE", "sw1.test.example")
        .env("JASPY_DISCOVERY_COMMUNITY", COMMUNITY)
        .env("JASPY_DISCOVERY_DNS_DOMAINS", "test.example")
        .env("JASPY_DISCOVERY_INTERVAL_SECS", "1")
        .start();

    // Periodic mode: at least two full runs happen without any manual trigger.
    assert!(
        wait_until(Duration::from_secs(20), || ifxtable.hits() >= 2),
        "expected >= 2 periodic discovery runs; hits={} log: {}", ifxtable.hits(), nexus.log()
    );

    // Config is env-seeded and periodic can be disabled at runtime.
    let mut config = nexus.get_json("/dev/discovery/config");
    assert_eq!(config["rootDevice"], "sw1.test.example");
    assert_eq!(config["periodicEnabled"], json!(true));
    config["periodicEnabled"] = json!(false);
    let resp = nexus.put_json("/dev/discovery/config", &config);
    assert!(resp.status().is_success());

    // Let any in-flight run drain, then confirm no further runs start.
    std::thread::sleep(Duration::from_millis(1500));
    let settled_hits = ifxtable.hits();
    std::thread::sleep(Duration::from_millis(3000));
    assert_eq!(ifxtable.hits(), settled_hits, "periodic runs should stop after disabling via PUT /dev/discovery/config");
}

// ---------------------------------------------------------------------------
// 1d. Web admin UI API (/api/v1) and embedded SPA serving
// ---------------------------------------------------------------------------

#[test]
fn api_v1_summary_and_devices() {
    let pg = PgHarness::start();
    let nexus = Nexus::builder(&pg.db_url).start();

    nexus.put_json("/dev/discovery/device", &discovery_body("sw1", "test.example"));

    let summary = nexus.get_json("/api/v1/summary");
    assert_eq!(summary["deviceCount"], json!(1), "summary: {:?}", summary);
    assert_eq!(summary["version"], json!("2.2.0"));
    assert!(summary["eventName"].is_null());
    assert!(summary["stateId"].as_i64().unwrap() > 0);
    assert!(summary["discovery"]["running"] == json!(false));

    let devices = nexus.get_json("/api/v1/devices");
    let list = devices.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["fqdn"], json!(FQDN));
    assert_eq!(list[0]["interfaceCount"], json!(2));
    assert!(list[0].get("up").is_some());

    // Unconfigured discovery run is rejected with a human-readable reason
    // (the web UI surfaces the `error` field verbatim).
    let resp = nexus.post_json("/api/v1/discovery/run", &json!({}));
    assert_eq!(resp.status().as_u16(), 400);
    let body: serde_json::Value = resp.json().unwrap();
    assert!(
        body["error"].as_str().unwrap_or("").contains("not configured"),
        "400 body should explain the problem: {:?}", body
    );

    let detail = nexus.get_json(&format!("/api/v1/devices/{}", FQDN));
    assert_eq!(detail["device"]["fqdn"], json!(FQDN));
    let interfaces = detail["interfaces"].as_array().unwrap();
    assert_eq!(interfaces.len(), 2);
    assert_eq!(interfaces[0]["name"], json!("GigabitEthernet0/1"));

    // Polling toggle via PUT mirrors /dev/device semantics.
    let mut update = json!({
        "name": "sw1", "dnsDomain": "test.example", "snmpCommunity": COMMUNITY,
        "baseMac": null, "pollingEnabled": false, "osInfo": null,
        "deviceType": null, "softwareVersion": null
    });
    let resp = nexus.put_json(&format!("/api/v1/devices/{}", FQDN), &update);
    assert!(resp.status().is_success());
    let devices = nexus.get_json("/api/v1/devices");
    assert_eq!(devices.as_array().unwrap()[0]["pollingEnabled"], json!(false));
    update["pollingEnabled"] = json!(null);
    nexus.put_json(&format!("/api/v1/devices/{}", FQDN), &update);
}

#[test]
fn api_v1_event_and_reset() {
    let pg = PgHarness::start();
    let nexus = Nexus::builder(&pg.db_url).start();

    // Event name round-trips and shows up in the summary.
    let resp = nexus.put_json("/api/v1/event", &json!({"name": "Test LAN 2026"}));
    assert!(resp.status().is_success());
    assert_eq!(nexus.get_json("/api/v1/event")["name"], json!("Test LAN 2026"));
    assert_eq!(nexus.get_json("/api/v1/summary")["eventName"], json!("Test LAN 2026"));

    // Persist a discovery config so we can assert reset keeps it.
    let mut config = nexus.get_json("/api/v1/discovery/config");
    config["rootDevice"] = json!("root.test.example");
    nexus.put_json("/api/v1/discovery/config", &config);

    // Seed topology + a client location.
    nexus.put_json("/dev/discovery/device", &discovery_body("sw1", "test.example"));
    nexus.put_json("/dev/discovery/device", &discovery_body("sw2", "test.example"));
    nexus.post_json(
        "/dev/device",
        &json!({"name":"sw3","dnsDomain":"test.example","snmpCommunity":COMMUNITY,"baseMac":"aa:bb:cc:dd:ee:ff","pollingEnabled":true}),
    );
    nexus.put_json("/dev/clientlocation/", &json!({
        "yiaddr": "10.1.2.3", "chaddr": "11:22:33:44:55:66",
        "option82": {"001": "00:00:00:00:0a:05", "002": "00:00:aa:bb:cc:dd:ee:ff"}
    }));

    let locations = nexus.get_json("/api/v1/clientlocations");
    assert_eq!(locations.as_array().unwrap().len(), 1);

    // Reset wipes all four domain tables and the event name.
    let resp = nexus.post_json("/api/v1/reset", &json!({}));
    assert!(resp.status().is_success());
    let result: serde_json::Value = resp.json().unwrap();
    assert_eq!(result["devicesDeleted"], json!(3));

    let mut conn = pg.conn();
    #[derive(QueryableByName)]
    struct CountRow {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
    }
    for table in ["devices", "interfaces", "client_locations", "weathermap_device_infos"] {
        let rows: Vec<CountRow> = query_rows(&mut conn, &format!("select count(*) as n from {}", table));
        assert_eq!(rows[0].n, 0, "table {} should be empty after reset", table);
    }
    assert!(nexus.get_json("/api/v1/event")["name"].is_null(), "event name should be cleared");
    assert_eq!(
        nexus.get_json("/api/v1/discovery/config")["rootDevice"],
        json!("root.test.example"),
        "discovery config must survive a reset"
    );

    // IMDS must purge the deleted devices (JASPY_IMDS_REFRESH_SECS=1 in the
    // harness) so metrics stop being exported for the wiped fleet.
    assert!(
        wait_until(Duration::from_secs(10), || !nexus.metrics_fast().contains("fqdn=\"sw1.test.example\"")),
        "IMDS should drop deleted devices from metrics after reset; metrics:\n{}",
        nexus.metrics_fast()
    );
}

#[test]
fn discovery_config_persists_across_restart() {
    let pg = PgHarness::start();

    {
        let nexus = Nexus::builder(&pg.db_url)
            .env("JASPY_DISCOVERY_ROOT_DEVICE", "from-env.test.example")
            .start();
        let mut config = nexus.get_json("/api/v1/discovery/config");
        assert_eq!(config["rootDevice"], json!("from-env.test.example"));
        config["rootDevice"] = json!("from-ui.test.example");
        config["community"] = json!("uicomm");
        let resp = nexus.put_json("/api/v1/discovery/config", &config);
        assert!(resp.status().is_success());
    } // nexus dropped (killed)

    // Same DB, same env seed: the persisted config must win over env.
    let nexus = Nexus::builder(&pg.db_url)
        .env("JASPY_DISCOVERY_ROOT_DEVICE", "from-env.test.example")
        .start();
    let config = nexus.get_json("/api/v1/discovery/config");
    assert_eq!(config["rootDevice"], json!("from-ui.test.example"));
    assert_eq!(config["community"], json!("uicomm"));
}

#[test]
fn spa_and_fallback() {
    let pg = PgHarness::start();
    let nexus = Nexus::builder(&pg.db_url).start();

    // Root serves the SPA shell (placeholder from build.rs is enough).
    let root = nexus.client.get(format!("{}/", nexus.base_url)).send().unwrap();
    assert_eq!(root.status().as_u16(), 200);
    let content_type = root.headers().get("content-type").unwrap().to_str().unwrap().to_string();
    assert!(content_type.contains("text/html"), "content-type: {}", content_type);
    let body = root.text().unwrap();
    assert!(body.contains("<html"), "root should serve the SPA shell");

    // Unknown client-side route falls back to the same shell.
    let fallback = nexus.client.get(format!("{}/devices/sw1.test.example", nexus.base_url)).send().unwrap();
    assert_eq!(fallback.status().as_u16(), 200);
    assert_eq!(fallback.text().unwrap(), body, "SPA fallback should serve index.html");

    // API namespaces must NOT fall back to HTML.
    assert_eq!(nexus.get_status("/api/v1/nonexistent"), 404);
    assert_eq!(nexus.get_status("/dev/nonexistent"), 404);

    // Missing files (extension in last segment) are real 404s, not the shell;
    // same for the weathermap prefix (FileServer misses must not become HTML).
    assert_eq!(nexus.get_status("/assets/no-such-file.js"), 404);
    assert_eq!(nexus.get_status("/weathermap/js/config.js"), 404);
}

// ---------------------------------------------------------------------------
// 1e. `jaspy-nexus trap-handler` subcommand (snmptrapd traphandle)
// ---------------------------------------------------------------------------

/// Run the trap-handler subcommand with the given stdin fixture, as snmptrapd
/// would. Stdout/stderr are nulled: the handler forks and the child would
/// otherwise hold the pipes open.
fn run_trap_handler(jaspy_url: &str, fixture: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new(env!("CARGO_BIN_EXE_jaspy-nexus"))
        .arg("trap-handler")
        .env("JASPY_URL", jaspy_url)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn trap-handler");
    child.stdin.as_mut().unwrap().write_all(read_fixture(fixture).as_bytes()).unwrap();
    let status = child.wait().expect("wait trap-handler");
    assert!(status.success(), "trap-handler should exit 0");
}

#[test]
fn trap_handler_reports_link_state() {
    let pg = PgHarness::start();
    let broker = MqttBroker::start();
    let nexus = Nexus::builder(&pg.db_url).mqtt(&broker.server()).start();

    nexus.put_json("/dev/discovery/device", &discovery_body("sw1", "test.example"));
    // IMDS must know the device+interfaces before reports are accepted.
    nexus.wait_ok(&format!("/dev/device/{}/status", FQDN), Duration::from_secs(10));

    // First report seeds the state (silently, by design): link down.
    run_trap_handler(&nexus.base_url, "trap_linkdown.txt");
    assert!(
        wait_until(Duration::from_secs(10), || {
            metric_value(&nexus.metrics_fast(), "jaspy_interface_up", &["name=\"GigabitEthernet0/1\""]) == Some(0)
        }),
        "linkDown trap should set the interface down; metrics:\n{}",
        nexus.metrics_fast()
    );

    // Second report flips the state and must emit an interfaceUpDown event.
    run_trap_handler(&nexus.base_url, "trap_linkup.txt");
    assert!(
        wait_until(Duration::from_secs(10), || {
            metric_value(&nexus.metrics_fast(), "jaspy_interface_up", &["name=\"GigabitEthernet0/1\""]) == Some(1)
        }),
        "linkUp trap should set the interface up"
    );
    let event = broker.wait_for_event(Duration::from_secs(10), |t, p| {
        t == "jaspy/nexus/interfaceUpDown" && p.contains("GigabitEthernet0/1") && p.contains(FQDN)
    });
    assert!(event.is_some(), "expected interfaceUpDown MQTT event; got {:?}", broker.events());

    // Unknown-host trap: fire-and-forget, must exit 0 and change nothing.
    run_trap_handler(&nexus.base_url, "trap_unknown_host.txt");
    assert_eq!(
        metric_value(&nexus.metrics_fast(), "jaspy_interface_up", &["name=\"GigabitEthernet0/1\""]),
        Some(1)
    );
}

// ---------------------------------------------------------------------------
// 2. Discovery writes devices + interfaces to Postgres
// ---------------------------------------------------------------------------
#[derive(QueryableByName)]
struct NameRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    dns_domain: String,
}

#[derive(QueryableByName)]
struct IfaceRow {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    ifindex: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
}

#[test]
fn discovery_writes_to_postgres() {
    let pg = PgHarness::start();
    let nexus = Nexus::builder(&pg.db_url).start();

    nexus.put_json("/dev/discovery/device", &discovery_body("sw1", "test.example"));

    let mut conn = pg.conn();
    let devs: Vec<NameRow> = query_rows(&mut conn, "select name, dns_domain from devices");
    assert_eq!(devs.len(), 1);
    assert_eq!(devs[0].name, "sw1");
    assert_eq!(devs[0].dns_domain, "test.example");

    let ifs: Vec<IfaceRow> =
        query_rows(&mut conn, "select \"index\" as ifindex, name from interfaces order by \"index\"");
    assert_eq!(ifs.len(), 2);
    assert_eq!(ifs[0].ifindex, 10101);
    assert_eq!(ifs[0].name, "GigabitEthernet0/1");
    assert_eq!(ifs[1].ifindex, 10102);

    // and it's visible over the API
    let ifaces = nexus.get_json(&format!("/dev/device/{}/interfaces", FQDN));
    assert_eq!(ifaces.as_array().unwrap().len(), 2);
}

// ---------------------------------------------------------------------------
// 3. Discovery links write topology to Postgres and surface in weathermap
// ---------------------------------------------------------------------------
#[derive(QueryableByName)]
struct ConnRow {
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
    connected_interface: Option<i32>,
}

#[test]
fn links_and_weathermap() {
    let pg = PgHarness::start();
    let nexus = Nexus::builder(&pg.db_url).start();

    nexus.put_json("/dev/discovery/device", &discovery_body("sw1", "test.example"));
    nexus.put_json("/dev/discovery/device", &discovery_body("sw2", "test.example"));

    let links = json!({
        "deviceFqdn": "sw1.test.example",
        "topologyStable": false,
        "interfaces": {
            "GigabitEthernet0/1": {"name":"sw2","dnsDomain":"test.example","interface":"GigabitEthernet0/1"}
        }
    });
    let resp = nexus.put_json("/dev/discovery/links", &links);
    assert!(resp.status().is_success());

    // Postgres: sw1's Gi0/1 now points at a peer interface.
    let mut conn = pg.conn();
    let rows: Vec<ConnRow> = query_rows(
        &mut conn,
        "select i.connected_interface from interfaces i \
         join devices d on d.id = i.device_id \
         where d.name = 'sw1' and i.name = 'GigabitEthernet0/1'",
    );
    assert_eq!(rows.len(), 1);
    assert!(rows[0].connected_interface.is_some(), "link should be recorded in DB");

    // Weathermap reflects the link.
    let wmap = nexus.get_json("/dev/weathermap/");
    let connected = &wmap["devices"]["sw1.test.example"]["interfaces"]["GigabitEthernet0/1"]["connectedTo"];
    assert_eq!(connected["fqdn"], "sw2.test.example");
    assert_eq!(connected["interface"], "GigabitEthernet0/1");
}

// ---------------------------------------------------------------------------
// 4. Device up/down metric via the monitor ingest path (pinger's report path)
// ---------------------------------------------------------------------------
#[test]
fn device_up_metric() {
    let pg = PgHarness::start();
    let nexus = Nexus::builder(&pg.db_url).start();

    nexus.post_json("/dev/device", &device_body(true));
    // Wait for IMDS to learn the device (report_device drops unknown devices).
    nexus.wait_ok(&format!("/dev/device/{}/status", FQDN), Duration::from_secs(10));

    nexus.put_json("/dev/device/monitor", &json!({"fqdn": FQDN, "up": true}));
    assert!(
        wait_until(Duration::from_secs(5), || {
            metric_value(&nexus.metrics_fast(), "jaspy_device_up", &[&format!("fqdn=\"{}\"", FQDN)]) == Some(1)
        }),
        "jaspy_device_up should become 1"
    );

    nexus.put_json("/dev/device/monitor", &json!({"fqdn": FQDN, "up": false}));
    assert!(
        wait_until(Duration::from_secs(5), || {
            metric_value(&nexus.metrics_fast(), "jaspy_device_up", &[&format!("fqdn=\"{}\"", FQDN)]) == Some(0)
        }),
        "jaspy_device_up should become 0"
    );
}

// ---------------------------------------------------------------------------
// 5. Client-location ingest writes to Postgres
// ---------------------------------------------------------------------------
#[derive(QueryableByName)]
struct ClientLocRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    ip_address: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    port_info: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    hw_address: String,
}

#[test]
fn client_location_writes_to_postgres() {
    let pg = PgHarness::start();
    let nexus = Nexus::builder(&pg.db_url).start();

    // Device whose base_mac matches the option-82 "002"-derived switch MAC.
    nexus.post_json(
        "/dev/device",
        &json!({"name":"sw1","dnsDomain":"test.example","snmpCommunity":COMMUNITY,"baseMac":"aa:bb:cc:dd:ee:ff","pollingEnabled":true}),
    );

    // option82 001: module=0x0a=10, port=0x05=5 -> "10/5"
    // option82 002: octets[2..8] -> "aa:bb:cc:dd:ee:ff"
    let payload = json!({
        "yiaddr": "10.1.2.3",
        "chaddr": "11:22:33:44:55:66",
        "option82": {
            "001": "00:00:00:00:0a:05",
            "002": "00:00:aa:bb:cc:dd:ee:ff"
        }
    });
    let resp = nexus.put_json("/dev/clientlocation/", &payload);
    assert!(resp.status().is_success());

    let mut conn = pg.conn();
    let rows: Vec<ClientLocRow> = query_rows(
        &mut conn,
        "select ip_address, port_info, hw_address from client_locations",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].ip_address, "10.1.2.3");
    assert_eq!(rows[0].port_info, "10/5");
    assert_eq!(rows[0].hw_address, "11:22:33:44:55:66");
}

// ---------------------------------------------------------------------------
// 6. MQTT events are published on device changes
// ---------------------------------------------------------------------------
#[test]
fn mqtt_events_published() {
    let pg = PgHarness::start();
    let broker = MqttBroker::start();
    let nexus = Nexus::builder(&pg.db_url).mqtt(&broker.server()).start();

    // Creating a device publishes jaspy/nexus/deviceCreated.
    nexus.post_json("/dev/device", &device_body(true));
    let created = broker.wait_for_event(Duration::from_secs(10), |t, _| t == "jaspy/nexus/deviceCreated");
    assert!(created.is_some(), "expected deviceCreated event; collected: {:?}", broker.events());
    let (topic, payload) = created.unwrap();
    assert_eq!(topic, "jaspy/nexus/deviceCreated");
    assert!(payload.contains(FQDN), "payload should reference {}: {}", FQDN, payload);

    // Toggling pollingEnabled publishes jaspy/nexus/devicePollingChanged.
    nexus.put_json(&format!("/dev/device/{}", FQDN), &device_body(false));
    let changed = broker.wait_for_event(Duration::from_secs(10), |t, _| t == "jaspy/nexus/devicePollingChanged");
    assert!(changed.is_some(), "expected devicePollingChanged event; collected: {:?}", broker.events());
}

// ---------------------------------------------------------------------------
// 7. Counter rollback is rejected (IMDS::validate_counters)
// ---------------------------------------------------------------------------
#[test]
fn counter_rollback_is_rejected() {
    let pg = PgHarness::start();
    let mock = SnmpbotMock::start();
    mock.stub_table(FQDN, COMMUNITY, "IF-MIB::ifTable", &read_fixture("iftable.json"));
    let mut ifx = mock.stub_table(FQDN, COMMUNITY, "IF-MIB::ifXTable", &read_fixture("ifxtable.json"));

    let nexus = Nexus::builder(&pg.db_url)
        .snmpbot(&mock.url())
        .poller(true)
        .poll_loop_msecs(300)
        .start();

    nexus.post_json("/dev/device", &device_body(true));
    nexus.put_json("/dev/discovery/device", &discovery_body("sw1", "test.example"));

    // establish the baseline (1_000_000)
    assert!(wait_until(Duration::from_secs(15), || {
        metric_value(&nexus.metrics(), "jaspy_interface_octets", &["name=\"GigabitEthernet0/1\"", "direction=\"rx\""]) == Some(1_000_000)
    }));

    // switch ifXTable to a rolled-back counter (10) and give the poller a few
    // cycles; the rollback must be rejected, so the value stays at 1_000_000.
    ifx.delete();
    mock.stub_table(FQDN, COMMUNITY, "IF-MIB::ifXTable", &read_fixture("ifxtable_rollback.json"));

    let regressed = wait_until(Duration::from_millis(1500), || {
        metric_value(&nexus.metrics(), "jaspy_interface_octets", &["name=\"GigabitEthernet0/1\"", "direction=\"rx\""]) == Some(10)
    });
    assert!(!regressed, "counter rollback should have been rejected; metric must not drop to 10");
    assert_eq!(
        metric_value(&nexus.metrics(), "jaspy_interface_octets", &["name=\"GigabitEthernet0/1\"", "direction=\"rx\""]),
        Some(1_000_000)
    );
}
