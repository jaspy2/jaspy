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
    assert_eq!(metric_value(&body, "jaspy_stp_port_state", &stp_labels), Some(5), "forwarding");
    assert_eq!(metric_value(&body, "jaspy_stp_port_role", &stp_labels), Some(3), "designated");
    assert_eq!(metric_value(&body, "jaspy_stp_port_enabled", &stp_labels), Some(1), "enabled");
    assert_eq!(metric_value(&body, "jaspy_stp_port_designated_cost", &stp_labels), Some(4));
    assert_eq!(metric_value(&body, "jaspy_stp_port_path_cost", &stp_labels), Some(19));
    assert_eq!(metric_value(&body, "jaspy_stp_port_priority", &stp_labels), Some(128));
    assert_eq!(metric_value(&body, "jaspy_stp_port_forward_transitions", &stp_labels), Some(2));
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
