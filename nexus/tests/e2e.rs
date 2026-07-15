// End-to-end tests: boot the real jaspy-nexus binary against a mock snmpbot and
// an ephemeral Postgres, then assert on SNMP queries issued, rows written to
// Postgres, and Prometheus metrics exposed.
mod common;

use std::time::{Duration, Instant};

use diesel::prelude::*;
use serde_json::json;

use common::*;

// Every e2e test runs against both database backends: `e2e_both!(name)`
// generates `name::pg` and `name::sqlite` #[test] wrappers around a
// `fn name(db: DbHarness)` body. Filter one side with
// `cargo test --test e2e -- ::sqlite` (or `::pg`).
macro_rules! e2e_both {
    ($name:ident) => {
        mod $name {
            #[test]
            fn pg() {
                super::$name(crate::common::DbHarness::start(crate::common::Backend::Pg));
            }
            #[test]
            fn sqlite() {
                super::$name(crate::common::DbHarness::start(crate::common::Backend::Sqlite));
            }
        }
    };
}

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
e2e_both!(poller_queries_and_interface_metrics);
fn poller_queries_and_interface_metrics(db: DbHarness) {
    let mock = SnmpbotMock::start();
    let iftable = mock.stub_table(FQDN, COMMUNITY, "IF-MIB::ifTable", &read_fixture("iftable.json"));
    let ifxtable = mock.stub_table(FQDN, COMMUNITY, "IF-MIB::ifXTable", &read_fixture("ifxtable.json"));
    let other = mock.stub_other_tables();

    let nexus = Nexus::builder(db.db_url())
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

    // (c) with the pinger disabled, successful SNMP replies mark the device up
    assert_eq!(
        metric_value(&fast, "jaspy_device_up", &[&format!("fqdn=\"{}\"", FQDN)]),
        Some(1),
        "device answering SNMP should be reported up when the pinger is off"
    );

    // (d) the devices API exposes freshness of the last poll
    let devices_api = nexus.get_json("/api/v1/devices");
    let seconds = &devices_api.as_array().unwrap()[0]["secondsSinceLastPoll"];
    assert!(seconds.is_u64(), "secondsSinceLastPoll should be set after a poll: {:?}", devices_api);
    assert!(seconds.as_u64().unwrap() < 60);
}

// ---------------------------------------------------------------------------
// 1b. Entitypoller renders entity sensor + per-VLAN STP metrics
// ---------------------------------------------------------------------------
e2e_both!(entitypoller_sensor_and_stp_metrics);
fn entitypoller_sensor_and_stp_metrics(db: DbHarness) {
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

    // Bridge-level scalars, polled per-vlan as one walk of the jaspy scalar
    // view (fixture values in the shapes real snmpbot emits: spaced hex
    // bridge id, numeric TimeTicks).
    let bridge_scalars = mock.stub_host_table(&vlan_host, "BRIDGE-MIB::jaspyStpBridgeTable", &read_fixture("jaspystpbridgetable.json"));

    let nexus = Nexus::builder(db.db_url())
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

    // (c) the same store is served as structured JSON for the device detail
    //     page: camelCase contract + numeric role/state decoded to text.
    let entity = nexus.get_json(&format!("/api/v1/devices/{}/entity", FQDN));
    assert_eq!(entity["sensors"][0]["valueType"], "celsius");
    assert_eq!(entity["sensors"][0]["value"], 45.0);
    assert_eq!(entity["sensors"][0]["interfaceName"], "GigabitEthernet0/1");
    let port = &entity["stp"][0];
    assert_eq!(port["vlan"], 100);
    assert_eq!(port["stpPortId"], 5);
    assert_eq!(port["state"], "forwarding");
    assert_eq!(port["role"], "designated");
    assert_eq!(port["enabled"], true);
    assert_eq!(port["pathCost"], 19);
    assert_eq!(port["designatedCost"], 4);
    assert_eq!(port["priority"], 128);

    // Unknown device: 200 with empty arrays, same as "no data yet".
    let empty = nexus.get_json("/api/v1/devices/ghost.test.example/entity");
    assert_eq!(empty["sensors"].as_array().unwrap().len(), 0);
    assert_eq!(empty["stp"].as_array().unwrap().len(), 0);

    // (d) bridge scalars: raw metrics, the per-device entity DTO, and the
    // network-wide STP endpoints built from the same store.
    assert!(bridge_scalars.hits() >= 1, "bridge scalars should have been queried as one batch");
    assert_eq!(metric_value(&body, "jaspy_stp_bridge_root_cost", &["vlan=\"100\""]), Some(20000));
    assert_eq!(metric_value(&body, "jaspy_stp_bridge_root_priority", &["root_mac=\"70:10:6f:63:f2:70\""]), Some(33068));
    // snmpbot TimeTicks are seconds (2297973 s ≈ 26.6 days on the live rig).
    assert_eq!(metric_value(&body, "jaspy_stp_bridge_time_since_topology_change", &["vlan=\"100\""]), Some(2297973));

    let bridge = &entity["stpBridges"][0];
    assert_eq!(bridge["vlan"], 100);
    assert_eq!(bridge["rootMac"], "70:10:6f:63:f2:70");
    assert_eq!(bridge["rootCost"], 20000);
    assert_eq!(bridge["rootPort"], 5);
    assert_eq!(bridge["rootPortInterfaceName"], "GigabitEthernet0/1");
    assert_eq!(bridge["topologyChanges"], 9);

    let summary = nexus.get_json("/api/v1/stp");
    let vlan100 = summary.as_array().unwrap().iter().find(|s| s["vlan"] == 100).expect("vlan 100 in /api/v1/stp");
    assert_eq!(vlan100["nodeCount"], 1);

    let tree = nexus.get_json("/api/v1/stp/100");
    assert_eq!(tree["nodes"].as_array().unwrap().len(), 1);
    let node = &tree["nodes"][0];
    assert_eq!(node["fqdn"], FQDN);
    assert_eq!(node["reported"]["rootMac"], "70:10:6f:63:f2:70");
    // The single node has a designated-only port set -> it is the root.
    assert_eq!(tree["roots"][0], FQDN);
}

// ---------------------------------------------------------------------------
// 1b². Vlanpoller: per-interface VLAN membership in the device detail API
// ---------------------------------------------------------------------------
e2e_both!(vlanpoller_vlans_in_device_detail);
fn vlanpoller_vlans_in_device_detail(db: DbHarness) {
    let mock = SnmpbotMock::start();

    // vlanpoller addresses snmpbot hosts inline as community@fqdn.
    let host = format!("{}@{}", COMMUNITY, FQDN);
    // jaspyVlanTrunkPortTable is the slim 6-column view of vlanTrunkPortTable
    // defined in snmpbot/mibs/CISCO-VTP-MIB.json (full-entry walks truncate on
    // slow switches).
    let trunk = mock.stub_host_table(&host, "CISCO-VTP-MIB::jaspyVlanTrunkPortTable", &read_fixture("vlantrunkporttable.json"));
    let vtp = mock.stub_host_table(&host, "CISCO-VTP-MIB::vtpVlanTable", &read_fixture("vtpvlantable.json"));
    mock.stub_host_table(&host, "CISCO-VLAN-MEMBERSHIP-MIB::vmMembershipTable", &read_fixture("vmmembershiptable.json"));

    let nexus = Nexus::builder(db.db_url())
        .snmpbot(&mock.url())
        .vlanpoller(true)
        .start();

    nexus.post_json("/dev/device", &device_body(true));
    nexus.put_json("/dev/discovery/device", &discovery_body("sw1", "test.example"));

    // The device detail response grows nativeVlan/taggedVlans once the poll
    // cycle has decoded the fixtures (interfaces are sorted by ifIndex:
    // [0] = 10101 trunk, [1] = 10102 access port).
    let ok = wait_until(Duration::from_secs(20), || {
        let detail = nexus.get_json(&format!("/api/v1/devices/{}", FQDN));
        detail["interfaces"][0]["nativeVlan"] == 300
    });
    let detail = nexus.get_json(&format!("/api/v1/devices/{}", FQDN));
    assert!(ok, "nativeVlan never appeared in device detail: {}", detail);

    assert!(trunk.hits() >= 1, "vlanTrunkPortTable should have been queried");
    assert!(vtp.hits() >= 1, "vtpVlanTable should have been queried");

    // Trunk port: native 300, tagged = active VLANs {1, 311} — the all-0xFF
    // allowed bitmap intersected with vtpVlanTable, minus the native VLAN.
    let trunk_if = &detail["interfaces"][0];
    assert_eq!(trunk_if["index"], 10101);
    assert_eq!(trunk_if["nativeVlan"], 300);
    assert_eq!(trunk_if["taggedVlans"], json!([1, 311]));

    // Access port: vmVlan 311, no tagged VLANs.
    let access_if = &detail["interfaces"][1];
    assert_eq!(access_if["index"], 10102);
    assert_eq!(access_if["nativeVlan"], 311);
    assert_eq!(access_if["taggedVlans"], json!([]));

    // The device-level VLAN catalog resolves ids to vtpVlanName values.
    let vlans = detail["vlans"].as_array().unwrap();
    assert!(vlans.contains(&json!({"id": 300, "name": "Mgmt"})), "vlans: {:?}", vlans);
    assert!(vlans.contains(&json!({"id": 311, "name": "Org"})), "vlans: {:?}", vlans);

    // The network-wide inventory aggregates the same store per VLAN id:
    // 311 is native on the access port (10102) and tagged on both trunking
    // rows (10101 and the port-channel 5001).
    let network = nexus.get_json("/api/v1/vlans");
    let v311 = network.as_array().unwrap().iter().find(|v| v["id"] == 311).expect("vlan 311 in /api/v1/vlans");
    assert_eq!(v311["names"], json!(["Org"]));
    assert_eq!(v311["devices"][0]["fqdn"], FQDN);
    assert_eq!(v311["devices"][0]["nativePorts"], 1);
    assert_eq!(v311["devices"][0]["taggedPorts"], 2);

    // Poll-now: known device queues (202), unknown device 404.
    let accepted = nexus.post_json(&format!("/api/v1/devices/{}/vlans/poll", FQDN), &json!({}));
    assert_eq!(accepted.status().as_u16(), 202);
    let missing = nexus.post_json("/api/v1/devices/ghost.test.example/vlans/poll", &json!({}));
    assert_eq!(missing.status().as_u16(), 404);
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
            // A second, bogus neighbor on the same port (LLDP flooded through
            // by a downstream device announcing a non-hostname sysname). It
            // must not shadow the real peer above during link resolution.
            json!({"HostID": fqdn, "Index": {"LLDP-MIB::lldpRemLocalPortNum": 1},
                   "Objects": {"LLDP-MIB::lldpRemSysName": "flooded junk", "LLDP-MIB::lldpRemChassisId": "de ad be ef 00 01",
                               "LLDP-MIB::lldpRemPortId": "eth9", "LLDP-MIB::lldpRemPortIdSubtype": "interfaceName"}}),
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

fn link_peer(conn: &mut TestConn, device: &str, interface: &str) -> Option<(String, String)> {
    let rows: Vec<PeerRow> = query_rows(conn, &format!(
        "select d2.name as peer_device, i2.name as peer_interface \
         from interfaces i \
         join devices d on d.id = i.device_id \
         join interfaces i2 on i2.id = i.connected_interface \
         join devices d2 on d2.id = i2.device_id \
         where d.name = '{}' and i.name = '{}'", device, interface));
    rows.into_iter().next().map(|r| (r.peer_device, r.peer_interface))
}

e2e_both!(discovery_engine_crawls_and_links);
fn discovery_engine_crawls_and_links(db: DbHarness) {
    let mock = SnmpbotMock::start();
    let broker = MqttBroker::start();

    // sw1 <-Gi0/1-> sw2 via LLDP (bare rem-sysnames, resolved through the
    // configured search domain); sw1 additionally announces sw2 via CDP.
    stub_discovery_device(&mock, "sw1.test.example", "01", Some("sw2"), Some("sw2.test.example"));
    stub_discovery_device(&mock, "sw2.test.example", "02", Some("sw1"), None);

    let nexus = Nexus::builder(db.db_url())
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
    assert_eq!(status["devicesFailed"], json!(0), "status: {:?}", status);
    assert_eq!(status["linksFound"], json!(2), "status: {:?}", status);
    assert!(status["lastError"].is_null(), "status: {:?}", status);

    // Live run log over WebSocket: the backlog is replayed on connect, so a
    // client connecting after the run still sees its full history.
    let ws_url = format!("{}/api/v1/ws/logs/discovery", nexus.base_url.replace("http://", "ws://"));
    let (mut socket, _) = tungstenite::connect(&ws_url).expect("websocket connect");
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_mut() {
        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    }
    let mut log_lines: Vec<String> = Vec::new();
    while let Ok(msg) = socket.read() {
        if let tungstenite::Message::Text(text) = msg {
            let entry: serde_json::Value = serde_json::from_str(&text).expect("log line should be json");
            assert!(entry["ts"].as_f64().is_some(), "log line without ts: {}", text);
            log_lines.push(entry["line"].as_str().unwrap_or_default().to_string());
        }
        if log_lines.iter().any(|l| l.contains("run finished")) {
            break;
        }
    }
    for expected in [
        "starting manual run (root=sw1.test.example",
        "[sw1.test.example] discovered: type WS-C2960X-24 (sw 15.2(2)E), 2 interfaces",
        "[sw2.test.example] discovered:",
        "2 devices discovered, 0 failed, 2 links",
    ] {
        assert!(
            log_lines.iter().any(|l| l.contains(expected)),
            "expected a log line containing {:?}; got: {:#?}", expected, log_lines
        );
    }

    // Devices + metadata landed in Postgres.
    let mut conn = db.conn();
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

e2e_both!(discovery_root_failure_sets_last_error);
fn discovery_root_failure_sets_last_error(db: DbHarness) {
    let mock = SnmpbotMock::start();
    let broker = MqttBroker::start();

    // No snmpbot stubs at all: every table fetch fails, so the root device
    // fails discovery and the run must surface that in lastError instead of
    // reporting a silent "finished, 0 devices".
    let nexus = Nexus::builder(db.db_url())
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

    let status = nexus.get_json("/dev/discovery/status");
    assert_eq!(status["devicesFound"], json!(0), "status: {:?}", status);
    assert_eq!(status["devicesFailed"], json!(1), "status: {:?}", status);
    let err = status["lastError"].as_str().unwrap_or_default();
    assert!(
        err.contains("root device sw1.test.example") && err.contains("IF-MIB::ifXTable"),
        "lastError should name the root device and the failed table; status: {:?}", status
    );
}

e2e_both!(discovery_periodic_runs_and_config_disable);
fn discovery_periodic_runs_and_config_disable(db: DbHarness) {
    let mock = SnmpbotMock::start();

    // Single device with no neighbors; periodic every second.
    let ifxtable = stub_discovery_device(&mock, "sw1.test.example", "01", None, None);

    let nexus = Nexus::builder(db.db_url())
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

e2e_both!(api_v1_summary_and_devices);
fn api_v1_summary_and_devices(db: DbHarness) {
    let nexus = Nexus::builder(db.db_url()).start();

    nexus.put_json("/dev/discovery/device", &discovery_body("sw1", "test.example"));

    // No JASPY_MQTT_SERVER in this harness config: startup must say so.
    assert!(nexus.log().contains("[mqtt] disabled (JASPY_MQTT_SERVER not set)"), "log:\n{}", nexus.log());

    // System status reflects the harness configuration (poller/pinger/mqtt off).
    let system = nexus.get_json("/api/v1/system");
    assert_eq!(system["pollerEnabled"], json!(false), "system: {:?}", system);
    assert_eq!(system["pingerEnabled"], json!(false), "system: {:?}", system);
    assert_eq!(system["deviceStatusSource"], json!("poller"), "system: {:?}", system);
    assert_eq!(system["mqttEnabled"], json!(false), "system: {:?}", system);

    // Database status matches the backend this matrix variant runs on, and
    // auto-migrate has left nothing pending.
    let expected_backend = match &db {
        DbHarness::Pg(_) => "postgresql",
        DbHarness::Sqlite(_) => "sqlite",
    };
    assert_eq!(system["dbBackend"], json!(expected_backend), "system: {:?}", system);
    assert_eq!(system["dbConnected"], json!(true), "system: {:?}", system);
    assert_eq!(system["dbMigrationsPending"], json!(false), "system: {:?}", system);
    assert_eq!(system["mqttBroker"], json!(null), "system: {:?}", system);
    assert_eq!(
        system["dbUrl"].as_str().unwrap_or_default(),
        db.db_url(),
        "dbUrl should match the harness (no password in either backend's test url); system: {:?}", system
    );

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

e2e_both!(api_v1_event_and_reset);
fn api_v1_event_and_reset(db: DbHarness) {
    let nexus = Nexus::builder(db.db_url()).start();

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

    let mut conn = db.conn();
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

e2e_both!(discovery_config_persists_across_restart);
fn discovery_config_persists_across_restart(db: DbHarness) {

    {
        let nexus = Nexus::builder(db.db_url())
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
    let nexus = Nexus::builder(db.db_url())
        .env("JASPY_DISCOVERY_ROOT_DEVICE", "from-env.test.example")
        .start();
    let config = nexus.get_json("/api/v1/discovery/config");
    assert_eq!(config["rootDevice"], json!("from-ui.test.example"));
    assert_eq!(config["community"], json!("uicomm"));
}

e2e_both!(spa_and_fallback);
fn spa_and_fallback(db: DbHarness) {
    let nexus = Nexus::builder(db.db_url()).start();

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

e2e_both!(trap_handler_reports_link_state);
fn trap_handler_reports_link_state(db: DbHarness) {
    let broker = MqttBroker::start();
    let nexus = Nexus::builder(db.db_url()).mqtt(&broker.server()).start();

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

    // Subscribe to the per-device live event topic BEFORE flipping the state:
    // device topics are live-only (no backlog replay). Brief pause so the
    // server-side subscription is registered after the handshake.
    let ws_url = format!("{}/api/v1/ws/logs/device:{}", nexus.base_url.replace("http://", "ws://"), FQDN);
    let (mut socket, _) = tungstenite::connect(&ws_url).expect("websocket connect");
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_mut() {
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    }
    std::thread::sleep(Duration::from_millis(250));

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

    // The same event must arrive on the per-device websocket topic (trap ->
    // IMDS -> msgbus -> livelog -> websocket).
    let mut ws_event: Option<serde_json::Value> = None;
    while let Ok(msg) = socket.read() {
        if let tungstenite::Message::Text(text) = msg {
            let parsed: serde_json::Value = serde_json::from_str(&text).expect("event frame should be json");
            if parsed["eventType"] == json!("interfaceUpDown") {
                ws_event = Some(parsed);
                break;
            }
        }
    }
    let ws_event = ws_event.expect("expected an interfaceUpDown frame on the device websocket topic");
    assert_eq!(ws_event["interfaceUpDown"]["fqdn"], json!(FQDN), "event: {:?}", ws_event);
    assert_eq!(ws_event["interfaceUpDown"]["name"], json!("GigabitEthernet0/1"), "event: {:?}", ws_event);
    assert_eq!(ws_event["interfaceUpDown"]["newState"], json!(true), "event: {:?}", ws_event);

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

e2e_both!(discovery_writes_to_postgres);
fn discovery_writes_to_postgres(db: DbHarness) {
    let nexus = Nexus::builder(db.db_url()).start();

    nexus.put_json("/dev/discovery/device", &discovery_body("sw1", "test.example"));

    let mut conn = db.conn();
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

e2e_both!(links_and_weathermap);
fn links_and_weathermap(db: DbHarness) {
    let nexus = Nexus::builder(db.db_url()).start();

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
    let mut conn = db.conn();
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

    // Device detail API: structured peer on the side that stores the FK...
    let detail = nexus.get_json("/api/v1/devices/sw1.test.example");
    let iface = detail["interfaces"].as_array().unwrap().iter()
        .find(|i| i["name"] == "GigabitEthernet0/1").unwrap().clone();
    assert_eq!(iface["connectedTo"]["fqdn"], "sw2.test.example", "iface: {:?}", iface);
    assert_eq!(iface["connectedTo"]["interface"], "GigabitEthernet0/1");
    // ...and derived from the reverse direction on the side that does not
    // (links are stored one-directionally).
    let detail = nexus.get_json("/api/v1/devices/sw2.test.example");
    let iface = detail["interfaces"].as_array().unwrap().iter()
        .find(|i| i["name"] == "GigabitEthernet0/1").unwrap().clone();
    assert_eq!(iface["connectedTo"]["fqdn"], "sw1.test.example", "reverse link missing; iface: {:?}", iface);
    assert_eq!(iface["connectedTo"]["interface"], "GigabitEthernet0/1");
}

// ---------------------------------------------------------------------------
// 4. Device up/down metric via the monitor ingest path (pinger's report path)
// ---------------------------------------------------------------------------
e2e_both!(device_up_metric);
fn device_up_metric(db: DbHarness) {
    let nexus = Nexus::builder(db.db_url()).start();

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

e2e_both!(client_location_writes_to_postgres);
fn client_location_writes_to_postgres(db: DbHarness) {
    let nexus = Nexus::builder(db.db_url()).start();

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

    let mut conn = db.conn();
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
e2e_both!(mqtt_events_published);
fn mqtt_events_published(db: DbHarness) {
    let broker = MqttBroker::start();
    let nexus = Nexus::builder(db.db_url()).mqtt(&broker.server()).start();

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

    // MQTT connection state is reported on stdout.
    let log = nexus.log();
    assert!(log.contains("[mqtt] enabled, publishing events to"), "log:\n{}", log);
    assert!(log.contains("[mqtt] connected to"), "log:\n{}", log);

    // ...and on the system status endpoint.
    let system = nexus.get_json("/api/v1/system");
    assert_eq!(system["mqttEnabled"], json!(true), "system: {:?}", system);
    assert!(
        system["mqttBroker"].as_str().unwrap_or_default().contains("127.0.0.1"),
        "system: {:?}", system
    );
    assert!(
        wait_until(Duration::from_secs(10), || {
            nexus.get_json("/api/v1/system")["mqttConnected"] == json!(true)
        }),
        "mqttConnected should become true; system: {:?}", nexus.get_json("/api/v1/system")
    );
}

// ---------------------------------------------------------------------------
// 7. Counter rollback is rejected (IMDS::validate_counters)
// ---------------------------------------------------------------------------
e2e_both!(counter_rollback_is_rejected);
fn counter_rollback_is_rejected(db: DbHarness) {
    let mock = SnmpbotMock::start();
    mock.stub_table(FQDN, COMMUNITY, "IF-MIB::ifTable", &read_fixture("iftable.json"));
    let mut ifx = mock.stub_table(FQDN, COMMUNITY, "IF-MIB::ifXTable", &read_fixture("ifxtable.json"));

    let nexus = Nexus::builder(db.db_url())
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

// ---------------------------------------------------------------------------
// 6. Mock mode: `jaspy-nexus mock` serves a full fake network end to end
// ---------------------------------------------------------------------------
e2e_both!(mock_mode_serves_network);
fn mock_mode_serves_network(db: DbHarness) {

    // External-DB path (no nested ephemeral postgres): the builder provides
    // JASPY_DB_URL, so mock mode migrates and uses it. Real collectors on,
    // fast intervals; the fake snmpbot binds a per-test free port.
    let nexus = Nexus::builder(db.db_url())
        .arg("mock")
        .poller(true)
        .poll_loop_msecs(300)
        .entitypoller(true)
        .entitypoller_interval_msecs(500)
        .env("JASPY_MOCK_SNMPBOT_PORT", &free_port().to_string())
        .env("JASPY_DISCOVERY_INTERVAL_SECS", "5")
        .start();

    let expected_fqdns = [
        "core1.mock.jaspy",
        "dist1.mock.jaspy",
        "dist2.mock.jaspy",
        "access-hall-a-01.mock.jaspy",
        "access-hall-a-02.mock.jaspy",
        "access-hall-b-01.mock.jaspy",
        "wlc1.mock.jaspy",
        "fw1.mock.jaspy",
    ];

    // The first periodic discovery run crawls the fake snmpbot and ingests
    // the whole topology.
    assert!(
        wait_until(Duration::from_secs(30), || {
            let devices = nexus.get_json("/api/v1/devices");
            let listed: Vec<String> = devices
                .as_array()
                .map(|list| list.iter().filter_map(|d| d["fqdn"].as_str().map(String::from)).collect())
                .unwrap_or_default();
            expected_fqdns.iter().all(|fqdn| listed.iter().any(|l| l == fqdn))
        }),
        "all mock devices should appear via /api/v1/devices; log:\n{}",
        nexus.log()
    );

    // Devices are ingested while the crawl is still running; devicesFound /
    // linksFound are only published when the run finishes, so wait for that.
    assert!(
        wait_until(Duration::from_secs(30), || {
            let status = nexus.get_json("/dev/discovery/status");
            status["running"] == false && status["lastFinished"].is_f64()
        }),
        "discovery run should finish; status: {}",
        nexus.get_json("/dev/discovery/status")
    );
    let status = nexus.get_json("/dev/discovery/status");
    assert_eq!(status["devicesFound"], 8, "status: {}", status);
    assert!(status["linksFound"].as_u64().unwrap_or(0) >= 6, "status: {}", status);

    // Poller flows counters from the fake snmpbot into /dev/metrics.
    nexus.wait_for_metric("jaspy_interface_octets", Duration::from_secs(20));
    let body = nexus.metrics();
    assert!(
        metric_value(&body, "jaspy_interface_octets", &["fqdn=\"core1.mock.jaspy\"", "name=\"Te1/0/1\"", "direction=\"rx\""]).is_some(),
        "core1 uplink octets missing:\n{}",
        body
    );

    // Entitypoller sensors from the fake snmpbot (both MIB styles feed the
    // same metric) and the structured per-device API.
    nexus.wait_for_metric("jaspy_sensors", Duration::from_secs(20));
    assert!(
        wait_until(Duration::from_secs(10), || {
            let entity = nexus.get_json("/api/v1/devices/dist2.mock.jaspy/entity");
            entity["sensors"].as_array().map(|s| !s.is_empty()).unwrap_or(false)
        }),
        "dist2 sensors should appear in the entity API"
    );

    // STP tree endpoints: vlan 10 is core1 -> dist1 -> {a-01, a-02} (4 nodes,
    // max depth 2); a-02's redundant backup uplink to core1 is the one
    // blocked link.
    assert!(
        wait_until(Duration::from_secs(30), || {
            let tree = nexus.get_json("/api/v1/stp/10");
            tree["nodes"].as_array().map(|n| n.len() == 4).unwrap_or(false)
                && tree["nodes"].as_array().unwrap().iter().all(|n| n["parent"].is_string() || n["depth"] == 0)
        }),
        "vlan 10 stp tree should converge to 4 linked nodes; log:\n{}",
        nexus.log()
    );
    let tree = nexus.get_json("/api/v1/stp/10");
    assert_eq!(tree["roots"], json!(["core1.mock.jaspy"]), "tree: {}", tree);
    let max_depth = tree["nodes"].as_array().unwrap().iter().map(|n| n["depth"].as_i64().unwrap()).max();
    assert_eq!(max_depth, Some(2));
    assert_eq!(tree["blockedLinks"].as_array().unwrap().len(), 1, "tree: {}", tree);
    assert_eq!(tree["blockedLinks"][0]["fqdn"], "access-hall-a-02.mock.jaspy");
    assert_eq!(tree["blockedLinks"][0]["connectedTo"]["fqdn"], "core1.mock.jaspy");
    assert_eq!(tree["flags"], json!([]), "healthy mock tree has no flags");
    // Reported root scalars agree with the computed root.
    assert!(tree["nodes"].as_array().unwrap().iter().all(|n| n["rootMismatch"] == false), "tree: {}", tree);

    // vlan 20: core1 -> dist2 -> b-01 chain.
    let tree20 = nexus.get_json("/api/v1/stp/20");
    let b01 = tree20["nodes"].as_array().unwrap().iter().find(|n| n["fqdn"] == "access-hall-b-01.mock.jaspy").expect("b-01 in vlan 20 tree");
    assert_eq!(b01["depth"], 2);
    assert_eq!(b01["parent"], "dist2.mock.jaspy");

    // The summary lists both vlans with core1 as root.
    let stp_summary = nexus.get_json("/api/v1/stp");
    let vlans: Vec<i64> = stp_summary.as_array().unwrap().iter().map(|s| s["vlan"].as_i64().unwrap()).collect();
    assert_eq!(vlans, vec![10, 20]);
    assert!(stp_summary.as_array().unwrap().iter().all(|s| s["rootFqdn"] == "core1.mock.jaspy"), "summary: {}", stp_summary);

    // Seeder: client locations attached to the access switches + event name.
    assert!(
        wait_until(Duration::from_secs(20), || {
            nexus.get_json("/api/v1/clientlocations").as_array().map(|c| c.len() >= 10).unwrap_or(false)
        }),
        "client locations should be seeded; log:\n{}",
        nexus.log()
    );
    assert_eq!(nexus.get_json("/api/v1/event")["name"], "Mock Event");

    // Devices report up (poller answered by the fake snmpbot).
    let summary = nexus.get_json("/api/v1/summary");
    assert!(summary["devicesUp"].as_u64().unwrap_or(0) >= 7, "summary: {}", summary);
}

// ---------------------------------------------------------------------------
// 7. Mock mode default database: sqlite temp file, zero prerequisites
// ---------------------------------------------------------------------------
#[test]
fn mock_mode_defaults_to_sqlite() {
    // No JASPY_DB_URL at all: mock mode must provision its own sqlite file.
    let nexus = Nexus::builder("")
        .no_db()
        .arg("mock")
        .poller(true)
        .poll_loop_msecs(300)
        .env("JASPY_MOCK_SNMPBOT_PORT", &free_port().to_string())
        .env("JASPY_DISCOVERY_INTERVAL_SECS", "5")
        .start();

    assert!(
        nexus.log().contains("[mock] sqlite database sqlite://"),
        "mock should announce its sqlite temp file; log:\n{}",
        nexus.log()
    );

    let system = nexus.get_json("/api/v1/system");
    assert_eq!(system["dbBackend"], json!("sqlite"), "system: {:?}", system);
    assert_eq!(system["dbConnected"], json!(true), "system: {:?}", system);

    // The discovery crawl works against sqlite end to end.
    assert!(
        wait_until(Duration::from_secs(30), || {
            nexus.get_json("/api/v1/devices").as_array().map(|d| d.len() == 8).unwrap_or(false)
        }),
        "mock devices should be ingested into sqlite; log:\n{}",
        nexus.log()
    );
}
