// The fake network served by `jaspy-nexus mock`: a small campus (core, two
// distribution switches, three access switches, a WLC and a firewall) with
// LLDP adjacency, sensors, per-VLAN STP and LACP port-channels (2×10G
// core↔dist bundles, plus one healthy and one misconfigured bundle on the
// access layer). Every value is a pure function of elapsed time since
// startup — counters grow, sensors drift, one uplink flaps — so there is no
// mutation thread and no locking.
//
// The generated tables mirror the snmpbot response shapes in tests/fixtures/
// exactly; they serialize through the same SNMPBotResponse structs the
// collectors deserialize, so shape compatibility holds by construction.
use crate::collectors::poller::{SNMPBotResponse, SNMPBotResultEntry};
use serde_json::json;

pub const DOMAIN: &str = "mock.jaspy";
pub const COMMUNITY: &str = "mock";
pub const ROOT_DEVICE: &str = "core1.mock.jaspy";
// Flapping uplink: up for FLAP_HALF_PERIOD_SECS, down for the same, repeat.
pub const FLAP_HALF_PERIOD_SECS: u64 = 60;
// Renegotiating port: full speed for this long, then 1/10th, repeat — so the
// per-interface health "speed renegotiation" signal has something to show.
pub const RENEG_HALF_PERIOD_SECS: u64 = 45;

#[derive(Clone, Copy, PartialEq)]
pub enum SensorStyle {
    Standard, // ENTITY-SENSOR-MIB::entPhySensorTable
    Cisco,    // CISCO-ENTITY-SENSOR-MIB::entSensorValueTable
    None,     // sensor tables answer 404
}

// Which VLAN membership MIBs the device answers (vlanpoller tries Cisco first,
// then falls back to Q-BRIDGE).
#[derive(Clone, Copy, PartialEq)]
pub enum VlanStyle {
    Cisco,   // CISCO-VTP-MIB + CISCO-VLAN-MEMBERSHIP-MIB
    QBridge, // Q-BRIDGE-MIB + un-vlan-indexed BRIDGE-MIB::dot1dBasePortTable
    None,    // VLAN tables answer 404
}

// Which spanning-tree MIBs the device answers (the entitypoller tries the
// Cisco stpx tables first, then falls back to HP-ICF-RPVST-MIB).
#[derive(Clone, Copy, PartialEq)]
pub enum StpStyle {
    Cisco, // CISCO-STP-EXTENSIONS-MIB + per-vlan BRIDGE-MIB (+ scalars)
    Rpvst, // HP-ICF-RPVST-MIB single-column views on the base community
    None,  // STP tables answer 404
}

pub struct MockInterface {
    pub ifindex: i64,
    pub name: &'static str,  // short form, becomes ifName ("Te1/0/1")
    pub descr: &'static str, // long form, becomes ifDescr
    pub alias: &'static str,
    pub speed_mbps: u64,
    // (bare peer device name, peer interface name); declared on one side,
    // emitted into LLDP tables symmetrically.
    pub peer: Option<(&'static str, &'static str)>,
    pub flaps: bool,
    // Static state for access ports without a peer (a realistic mix).
    pub up: bool,
    // Simulated fault knobs for the per-interface health signals. All default
    // to clean (0/false); a few ports set them (see build()) so the demo/e2e
    // exercises discards, errors, high utilization and speed renegotiation.
    pub discard_rate: u64, // ifOutDiscards per second
    pub error_rate: u64,   // ifInErrors per second
    pub saturated: bool,   // octet rate ≈ 95% of speed → high utilization
    pub renegotiates: bool, // ifHighSpeed toggles full↔1/10th
    // When set, the port is error-disabled: forced oper-down and reported in
    // CISCO-ERR-DISABLE-MIB with this cause (e.g. "bpduGuard").
    pub err_disable_cause: Option<&'static str>,
}

// A declared link aggregate for the LAG tables (pagpPortTable + dot3ad).
// Members with a topology peer report that peer's base MAC as their LACP
// partner (so bundles spanning two peers demo the mismatch warnings);
// unpeered members report `partner_mac` (an unmonitored server, say).
pub struct MockLag {
    pub ifindex: i64,
    pub members: &'static [i64],
    pub partner_mac: &'static str,
    // Members that report the `defaulted` LACP bit: sending LACPDUs but
    // hearing nothing back (far end not running LACP).
    pub defaulted_members: &'static [i64],
    // Members whose link is physically down (ifOperStatus down): the switch
    // still lists them as configured aggregate members (admin key matches) but
    // detached — the "a cable fell out of one uplink" demo.
    pub down_members: &'static [i64],
}

// PoE state for a PSE-capable switch: a switch-wide budget plus the copper
// access ports actively delivering power (with their real-time draw). Ports not
// listed are "searching" (no PD plugged). Feeds pethMainPseTable /
// pethPsePortTable / cpeExtPsePortTable (see the poe collector).
pub struct MockPoe {
    pub budget_w: i64,
    pub delivering: Vec<(i64, i64)>, // (ifindex, consumption_mw)
}

pub struct MockDevice {
    pub name: &'static str,
    pub model: &'static str,
    pub sys_descr: &'static str,
    pub sw_rev: &'static str,
    pub sensor_style: SensorStyle,
    pub stp_vlans: &'static [i64],
    pub stp_style: StpStyle,
    pub vlan_style: VlanStyle,
    // VLANs that exist on the device (vtpVlanTable / dot1qVlanCurrentTable).
    // Trunks (see is_trunk: peered interfaces + bundles over them) carry
    // native TRUNK_NATIVE_VLAN, tagged = the rest. The other ports are
    // access ports on access_vlan(ifindex).
    pub vlans: &'static [i64],
    pub interfaces: Vec<MockInterface>,
    pub lags: Vec<MockLag>,
    // PoE, for PSE-capable access switches; None on non-PoE devices.
    pub poe: Option<MockPoe>,
    // Whether the device publishes its own LLDP local-system data
    // (lldpLocChassisId + lldpLocPortTable) and a BRIDGE-MIB base address —
    // the sources discovery derives base_mac from, and the loc-port table it
    // uses to map LLDP neighbors onto local interfaces. FortiOS-style firewalls
    // (fw1) leave lldpLocalSystemData empty and aren't bridges, so they expose
    // none of these, yet still advertise neighbors in lldpRemTable. Setting this
    // false models that quirk: it exercises discovery's
    // lldpRemLocalPortNum==ifIndex fallback and yields a null base_mac.
    pub exposes_lldp_local: bool,
}

impl MockDevice {
    pub fn fqdn(&self) -> String {
        format!("{}.{}", self.name, DOMAIN)
    }
}

pub struct Topology {
    pub devices: Vec<MockDevice>,
    pub started: f64,
}

fn uplink(ifindex: i64, name: &'static str, descr: &'static str, alias: &'static str, speed: u64, peer: (&'static str, &'static str)) -> MockInterface {
    MockInterface { ifindex, name, descr, alias, speed_mbps: speed, peer: Some(peer), flaps: false, up: true,
        discard_rate: 0, error_rate: 0, saturated: false, renegotiates: false, err_disable_cause: None }
}

fn access_port(ifindex: i64, name: &'static str, descr: &'static str, up: bool) -> MockInterface {
    MockInterface { ifindex, name, descr, alias: "", speed_mbps: 1000, peer: None, flaps: false, up,
        discard_rate: 0, error_rate: 0, saturated: false, renegotiates: false, err_disable_cause: None }
}

// Aggregate (Po) interface: no LLDP peer of its own (LLDP runs on the
// members), speed = the bundle total.
fn port_channel(ifindex: i64, name: &'static str, descr: &'static str, alias: &'static str, speed: u64) -> MockInterface {
    MockInterface { ifindex, name, descr, alias, speed_mbps: speed, peer: None, flaps: false, up: true,
        discard_rate: 0, error_rate: 0, saturated: false, renegotiates: false, err_disable_cause: None }
}

pub fn build() -> Topology {
    let devices = vec![
        MockDevice {
            name: "core1",
            exposes_lldp_local: true,
            model: "C9606R",
            sys_descr: "Cisco IOS-XE Software (mock), Catalyst 9600 Switch",
            sw_rev: "17.9.4a",
            sensor_style: SensorStyle::Cisco,
            // core1 is the elected root for 10, 20 and 63. On VLAN 63 an
            // access switch (a-01) reports a *superior*, off-fleet root — the
            // stp-root-mismatch + stp-orphan demo (see object()). On VLAN 30 it
            // shares the root with dist1 (a directly-linked split brain).
            stp_vlans: &[10, 20, 30, 63],
            stp_style: StpStyle::Cisco,
            vlan_style: VlanStyle::Cisco,
            // A core switch trunks a lot of VLANs. This scattered set makes the
            // core1 uplinks carry ~14 tagged VLANs, exercising the device-detail
            // "N tagged VLANs" column summary (the full list stays in the
            // expandable per-interface detail).
            vlans: &[1, 10, 20, 30, 40, 50, 63, 100, 101, 102, 110, 200, 210, 300, 900, 999],
            interfaces: vec![
                // The dist downlinks are 2×10G LACP bundles (Po1/Po2 below);
                // both ends are monitored, so the far-end cross-checks run.
                uplink(10101, "Te1/0/1", "TenGigabitEthernet1/0/1", "downlink dist1", 10000, ("dist1", "Te1/1/1")),
                uplink(10102, "Te1/0/2", "TenGigabitEthernet1/0/2", "downlink dist2", 10000, ("dist2", "Te1/1/1")),
                uplink(10103, "Te1/0/3", "TenGigabitEthernet1/0/3", "firewall uplink", 10000, ("fw1", "port1")),
                // Redundant direct link to a-02: STP blocks it on a-02's end
                // (its backup uplink), giving trees a realistic blocked link.
                uplink(10104, "Te1/0/4", "TenGigabitEthernet1/0/4", "backup downlink hall a 02", 10000, ("access-hall-a-02", "Te1/1/2")),
                uplink(10105, "Te1/0/5", "TenGigabitEthernet1/0/5", "downlink dist1 (2)", 10000, ("dist1", "Te1/1/5")),
                uplink(10106, "Te1/0/6", "TenGigabitEthernet1/0/6", "downlink dist2 (2)", 10000, ("dist2", "Te1/1/5")),
                port_channel(5001, "Po1", "Port-channel1", "port-channel dist1", 20000),
                port_channel(5002, "Po2", "Port-channel2", "port-channel dist2", 20000),
            ],
            lags: vec![
                MockLag { ifindex: 5001, members: &[10101, 10105], partner_mac: "", defaulted_members: &[], down_members: &[] },
                MockLag { ifindex: 5002, members: &[10102, 10106], partner_mac: "", defaulted_members: &[], down_members: &[] },
            ],
            poe: None,
        },
        MockDevice {
            name: "dist1",
            exposes_lldp_local: true,
            model: "C9500-24Y4C",
            sys_descr: "Cisco IOS-XE Software (mock), Catalyst 9500 Switch",
            sw_rev: "17.9.4a",
            sensor_style: SensorStyle::Cisco,
            // Also runs VLAN 30, where it self-roots against core1 (split brain).
            stp_vlans: &[10, 30],
            stp_style: StpStyle::Cisco,
            vlan_style: VlanStyle::Cisco,
            vlans: &[1, 10, 30],
            interfaces: vec![
                uplink(10101, "Te1/1/1", "TenGigabitEthernet1/1/1", "uplink core1", 10000, ("core1", "Te1/0/1")),
                // Runs saturated: the far end (access-hall-a-01 Te1/1/1) is also
                // saturated, so both ends raise iface-high-util and the /issues
                // page combines them into one two-ended link row.
                MockInterface {
                    ifindex: 10102,
                    name: "Te1/1/2",
                    descr: "TenGigabitEthernet1/1/2",
                    alias: "downlink hall a 01",
                    speed_mbps: 10000,
                    peer: Some(("access-hall-a-01", "Te1/1/1")),
                    flaps: false,
                    up: true,
                    discard_rate: 0, error_rate: 0, saturated: true, renegotiates: false, err_disable_cause: None,
                },
                MockInterface {
                    ifindex: 10103,
                    name: "Te1/1/3",
                    descr: "TenGigabitEthernet1/1/3",
                    alias: "downlink hall a 02",
                    speed_mbps: 10000,
                    peer: Some(("access-hall-a-02", "Te1/1/1")),
                    flaps: true,
                    up: true,
                    discard_rate: 0, error_rate: 0, saturated: false, renegotiates: false, err_disable_cause: None,
                },
                uplink(10105, "Te1/1/5", "TenGigabitEthernet1/1/5", "uplink core1 (2)", 10000, ("core1", "Te1/0/5")),
                // "uplink ..." alias so stp_role makes the bundle the root port.
                port_channel(5001, "Po1", "Port-channel1", "uplink core1 port-channel", 20000),
            ],
            lags: vec![MockLag { ifindex: 5001, members: &[10101, 10105], partner_mac: "", defaulted_members: &[], down_members: &[] }],
            poe: None,
        },
        MockDevice {
            name: "dist2",
            exposes_lldp_local: true,
            model: "C9500-24Y4C",
            sys_descr: "Cisco IOS-XE Software (mock), Catalyst 9500 Switch",
            sw_rev: "17.6.5",
            sensor_style: SensorStyle::Standard,
            stp_vlans: &[20],
            stp_style: StpStyle::Cisco,
            vlan_style: VlanStyle::Cisco,
            vlans: &[1, 20],
            interfaces: vec![
                uplink(10101, "Te1/1/1", "TenGigabitEthernet1/1/1", "uplink core1", 10000, ("core1", "Te1/0/2")),
                uplink(10102, "Te1/1/2", "TenGigabitEthernet1/1/2", "downlink hall b 01", 10000, ("access-hall-b-01", "Te1/1/1")),
                uplink(10104, "Te1/1/4", "TenGigabitEthernet1/1/4", "wlc uplink", 10000, ("wlc1", "Te0/0/1")),
                // Second uplink member with its cable pulled: link down. The
                // bundle stays up on Te1/1/1, so this is the "one of two LACP
                // uplinks fell out" demo (see MockLag.down_members below).
                MockInterface {
                    ifindex: 10105,
                    name: "Te1/1/5",
                    descr: "TenGigabitEthernet1/1/5",
                    alias: "uplink core1 (2)",
                    speed_mbps: 10000,
                    peer: Some(("core1", "Te1/0/6")),
                    flaps: false,
                    up: false,
                    discard_rate: 0, error_rate: 0, saturated: false, renegotiates: false, err_disable_cause: None,
                },
                port_channel(5001, "Po1", "Port-channel1", "uplink core1 port-channel", 20000),
                // Single-member SFP+ port-channel to wlc1 (mirrors wlc1 Po1):
                // one healthy link, so the only warning is single-member.
                port_channel(5002, "Po2", "Port-channel2", "single-member wlc uplink (SFP+)", 10000),
            ],
            lags: vec![
                MockLag { ifindex: 5001, members: &[10101, 10105], partner_mac: "", defaulted_members: &[], down_members: &[10105] },
                MockLag { ifindex: 5002, members: &[10104], partner_mac: "", defaulted_members: &[], down_members: &[] },
            ],
            poe: None,
        },
        {
            // a-01 carries a 2-member LACP bundle to an unmonitored server. Both
            // members bundle fine (no LACP warning), but one has negotiated down
            // to 100M while its sibling runs at 1G — the classic faulty-cable
            // asymmetry, surfaced as the lag:speed-mismatch error.
            // a-01 also runs STP on VLAN 63, where its uplink leads to dist1 —
            // which does NOT run 63 — so its upstream is unresolved (orphan),
            // and it reports a superior off-fleet root (root-mismatch).
            let mut a01 = access_switch("access-hall-a-01", ("dist1", "Te1/1/2"), false, VlanStyle::Cisco, &[10, 63], StpStyle::Cisco);
            a01.interfaces.push(port_channel(5001, "Po1", "Port-channel1", "server bundle", 2000));
            a01.lags = vec![MockLag {
                ifindex: 5001,
                members: &[10201, 10202],
                partner_mac: "02:00:00:00:99:01",
                defaulted_members: &[],
                down_members: &[],
            }];
            // The degraded bundle leg: Gi1/0/2 fell back to 100M (faulty cable).
            set_iface(&mut a01, 10202, |i| i.speed_mbps = 100);
            // Saturate the uplink to dist1 (Te1/1/2). The dist1 side is
            // saturated too, so both ends raise iface-high-util and the /issues
            // page shows a single combined "access-hall-a-01 ↔ dist1" row.
            set_iface(&mut a01, 10101, |i| i.saturated = true);
            // Simulated faults (non-LAG, up ports): a congested port dropping
            // ~200 discards/s and a flaky-cable port taking ~5 input errors/s.
            set_iface(&mut a01, 10204, |i| i.discard_rate = 200);
            set_iface(&mut a01, 10205, |i| i.error_rate = 5);
            // Gi1/0/3 got a switch plugged into an access port: BPDU guard
            // error-disabled it. Forced oper-down + reported in the err-disable
            // MIB, so it surfaces as the err-disabled badge and an /issues entry.
            set_iface(&mut a01, 10203, |i| i.err_disable_cause = Some("bpduGuard"));
            a01
        },
        {
            // a-02 gets a second, redundant uplink straight to core1; STP
            // keeps the dist1 path and blocks this one (see stp_role).
            let mut a02 = access_switch("access-hall-a-02", ("dist1", "Te1/1/3"), true, VlanStyle::Cisco, &[10], StpStyle::Cisco);
            a02.interfaces.insert(1, uplink(10102, "Te1/1/2", "TenGigabitEthernet1/1/2", "backup uplink core1", 10000, ("core1", "Te1/0/4")));
            // Misconfigured bundle over the two uplinks: they land on
            // different devices, and the core1 side is not running LACP —
            // demo data for the port-channel warnings.
            // "uplink ..." alias: the bundle takes the root-port role that its
            // bundled member (the primary uplink) folds into.
            a02.interfaces.push(port_channel(5001, "Po1", "Port-channel1", "uplink port-channel", 20000));
            a02.lags = vec![MockLag {
                ifindex: 5001,
                members: &[10101, 10102],
                partner_mac: "",
                defaulted_members: &[10102],
                down_members: &[],
            }];
            a02
        },
        // hall b answers only the standards-based Q-BRIDGE tables so the mock
        // exercises the vlanpoller's fallback path.
        // hall b speaks HP RPVST+ (like a ProCurve) so the mock exercises the
        // entitypoller's STP fallback path too.
        {
            let mut b01 = access_switch("access-hall-b-01", ("dist2", "Te1/1/2"), false, VlanStyle::QBridge, &[20], StpStyle::Rpvst);
            // Simulated faults: a saturated port (~95% util) and a port whose
            // link keeps renegotiating its speed (failing SFP/duplex).
            set_iface(&mut b01, 10201, |i| i.saturated = true);
            set_iface(&mut b01, 10202, |i| i.renegotiates = true);
            // A full closet: 7 PDs drawing 112 W against a 124 W budget (~90%),
            // so this device raises the poe-budget Issue (warn) for the demo/e2e.
            b01.poe = Some(MockPoe {
                budget_w: 124,
                delivering: vec![
                    (10201, 16000), (10202, 16000), (10203, 16000), (10204, 16000),
                    (10205, 16000), (10206, 16000), (10207, 16000),
                ],
            });
            b01
        },
        MockDevice {
            name: "wlc1",
            exposes_lldp_local: true,
            model: "AIR-CT5520-K9",
            sys_descr: "Cisco 5520 Series Wireless LAN Controller (mock)",
            sw_rev: "8.10.185.0",
            sensor_style: SensorStyle::None,
            stp_vlans: &[],
            stp_style: StpStyle::None,
            vlan_style: VlanStyle::None,
            vlans: &[],
            interfaces: vec![
                uplink(1, "Te0/0/1", "TenGigE0/0/1", "uplink dist2", 10000, ("dist2", "Te1/1/4")),
                // A single SFP+ link built as a port-channel — the common
                // "one-member Po" the single-member Issue is about. dist2 mirrors
                // it (Po2 below), so the bundle is symmetric and the only warning
                // is single-member.
                port_channel(5001, "Po1", "Port-channel1", "single-member uplink dist2 (SFP+)", 10000),
            ],
            lags: vec![MockLag { ifindex: 5001, members: &[1], partner_mac: "", defaulted_members: &[], down_members: &[] }],
            poe: None,
        },
        MockDevice {
            name: "fw1",
            // FortiOS: advertises LLDP neighbors but no lldpLocalSystemData and
            // no BRIDGE-MIB — see exposes_lldp_local on MockDevice.
            exposes_lldp_local: false,
            model: "FGT-900D",
            sys_descr: "Mock firewall appliance",
            sw_rev: "7.2.8",
            sensor_style: SensorStyle::None,
            stp_vlans: &[],
            stp_style: StpStyle::None,
            vlan_style: VlanStyle::None,
            vlans: &[],
            interfaces: vec![
                uplink(1, "port1", "port1", "uplink core1", 10000, ("core1", "Te1/0/3")),
            ],
            lags: Vec::new(),
            poe: None,
        },
    ];
    Topology { devices, started: crate::utilities::tools::get_time() }
}

// Mutate one interface (by ifindex) in place — used in build() to paint the
// simulated-fault ports onto the otherwise-clean access switches.
fn set_iface<F: FnOnce(&mut MockInterface)>(dev: &mut MockDevice, ifindex: i64, f: F) {
    if let Some(iface) = dev.interfaces.iter_mut().find(|i| i.ifindex == ifindex) {
        f(iface);
    }
}

fn access_switch(name: &'static str, upstream: (&'static str, &'static str), uplink_flaps: bool, vlan_style: VlanStyle, stp_vlans: &'static [i64], stp_style: StpStyle) -> MockDevice {
    let mut interfaces = vec![MockInterface {
        ifindex: 10101,
        name: "Te1/1/1",
        descr: "TenGigabitEthernet1/1/1",
        alias: "uplink",
        speed_mbps: 10000,
        peer: Some(upstream),
        flaps: uplink_flaps,
        up: true,
        discard_rate: 0, error_rate: 0, saturated: false, renegotiates: false, err_disable_cause: None,
    }];
    // Eight access ports; a deterministic mix of up/down.
    const PORTS: [(&str, &str); 8] = [
        ("Gi1/0/1", "GigabitEthernet1/0/1"),
        ("Gi1/0/2", "GigabitEthernet1/0/2"),
        ("Gi1/0/3", "GigabitEthernet1/0/3"),
        ("Gi1/0/4", "GigabitEthernet1/0/4"),
        ("Gi1/0/5", "GigabitEthernet1/0/5"),
        ("Gi1/0/6", "GigabitEthernet1/0/6"),
        ("Gi1/0/7", "GigabitEthernet1/0/7"),
        ("Gi1/0/8", "GigabitEthernet1/0/8"),
    ];
    for (i, (name, descr)) in PORTS.iter().enumerate() {
        interfaces.push(access_port(10201 + i as i64, name, descr, i % 3 != 2));
    }
    MockDevice {
        name,
        exposes_lldp_local: true,
        model: "C9300-48P",
        sys_descr: "Cisco IOS-XE Software (mock), Catalyst 9300 Switch",
        sw_rev: "17.6.5",
        sensor_style: SensorStyle::Standard,
        stp_vlans,
        stp_style,
        vlan_style,
        vlans: &[1, 10, 20],
        interfaces,
        lags: Vec::new(),
        // C9300-48P is a PoE switch: a couple of access ports power phones,
        // well under budget. build() bumps one hall near its budget to exercise
        // the poe-budget Issue.
        poe: Some(MockPoe {
            budget_w: 370,
            delivering: vec![(10201, 15400), (10203, 6500)],
        }),
    }
}

// entPhysicalIndex of a port's own "port"-class entity, mirroring the
// entPhysicalTable arm's numbering so cpeExtPsePortEntPhyIndex resolves back to
// the same interface (the join collectors::poe/entitypoller performs). Returns
// None for logical (Po) ports and empty SFP cages, which have no port entity.
// KEEP IN SYNC with the "ENTITY-MIB::entPhysicalTable" arm in table().
pub fn port_ent_index(dev: &MockDevice, ifindex: i64) -> Option<i64> {
    let mut ent_idx = 3001i64;
    for iface in dev.interfaces.iter() {
        if iface.name.starts_with("Po") {
            continue;
        }
        if iface.speed_mbps >= 10000 {
            ent_idx += 1; // SFP cage (container)
            if iface.ifindex % 3 != 0 {
                let optic_port = ent_idx;
                ent_idx += 1;
                if iface.ifindex == ifindex {
                    return Some(optic_port);
                }
            } else if iface.ifindex == ifindex {
                return None; // empty cage: no port entity
            }
        } else {
            if iface.ifindex == ifindex {
                return Some(ent_idx);
            }
            ent_idx += 1;
        }
    }
    None
}

// The device's PoE ports in index order: copper access ports (skip logical Po
// aggregates and 10G uplinks). Shared by the pethPsePort / cpeExtPsePort arms so
// both number ports identically.
fn poe_ports(dev: &MockDevice) -> Vec<&MockInterface> {
    dev.interfaces.iter()
        .filter(|i| !i.name.starts_with("Po") && i.speed_mbps < 10000)
        .collect()
}

// ---------------------------------------------------------------------------
// Time-derived values (all monotonic or bounded; IMDS rejects regressions)
// ---------------------------------------------------------------------------

pub fn base_mac(dev_idx: usize) -> String {
    format!("02:00:00:00:{:02x}:01", 0x10 + dev_idx)
}

// Fake management address (TEST-NET-2), shown on the device detail page via
// the mock's resolver override.
pub fn management_ip(dev_idx: usize) -> String {
    format!("198.51.100.{}", 10 + dev_idx)
}

fn base_mac_spaced(dev_idx: usize) -> String {
    base_mac(dev_idx).replace(":", " ")
}

fn iface_mac(dev_idx: usize, ifindex: i64) -> String {
    format!("02:00:00:{:02x}:{:02x}:{:02x}", 0x10 + dev_idx, (ifindex / 100) as u8, (ifindex % 100) as u8)
}

// Trunks (peered interfaces) use native VLAN 1; access ports alternate
// between VLANs 10 and 20 by ifindex.
pub const TRUNK_NATIVE_VLAN: i64 = 1;

pub fn access_vlan(iface: &MockInterface) -> i64 {
    10 + (iface.ifindex % 2) * 10
}

// Trunk-ness for the VLAN tables: discovered links are trunks, and a Po
// interface inherits it from its members (a bundle over switch-to-switch
// links trunks; a-01's server bundle stays an access port).
pub fn is_trunk(dev: &MockDevice, iface: &MockInterface) -> bool {
    if iface.peer.is_some() {
        return true;
    }
    dev.lags.iter().any(|lag| {
        lag.ifindex == iface.ifindex
            && lag.members.iter().any(|m| dev.interfaces.iter().any(|i| i.ifindex == *m && i.peer.is_some()))
    })
}

// Inverse of collectors::vlanpoller::bitmap_values, in snmpbot's OCTET STRING
// rendering (lowercase space-separated hex): bit j of octet i (MSB first)
// represents base + i*8 + j.
pub fn hex_bitmap(values: &[i64], base: i64, len: usize) -> String {
    let mut octets = vec![0u8; len];
    for value in values.iter() {
        let offset = value - base;
        if offset >= 0 && (offset as usize) < len * 8 {
            octets[(offset / 8) as usize] |= 0x80u8 >> (offset % 8);
        }
    }
    octets.iter().map(|o| format!("{:02x}", o)).collect::<Vec<String>>().join(" ")
}

// STP role of a peered interface: core1 is the root bridge (all designated);
// every primary uplink (access "uplink", dist "uplink core1") is the root
// port toward it — including a-02's flapping uplink, which stays a forwarding
// root port (mock STP is static). a-02's redundant "backup uplink core1" is
// the blocked alternate, so trees always show one physically consistent
// blocked link (blocked on a-02's end, designated on core1's).
pub fn stp_role(dev: &MockDevice, iface: &MockInterface) -> &'static str {
    if dev.name == "core1" {
        "designated"
    } else if iface.alias.starts_with("backup uplink") {
        "alternate"
    } else if iface.alias.starts_with("uplink") {
        "root"
    } else {
        "designated"
    }
}

// BRIDGE-MIB BridgeId in snmpbot's rendering: 2 priority bytes + the device's
// base MAC, space-separated lowercase hex.
pub fn bridge_id_spaced(priority: i64, dev_idx: usize) -> String {
    format!("{:02x} {:02x} {}", (priority >> 8) & 0xff, priority & 0xff, base_mac_spaced(dev_idx))
}

// A superior root (priority 4096 + vlan) advertised by a bridge that belongs
// to no mock device — drives the stp-root-mismatch demo. The MAC deliberately
// falls outside the mock's 02:00:00:00:1x:01 base-MAC space.
pub fn off_fleet_root_bridge_id(vlan: i64) -> String {
    let priority = 4096 + vlan;
    format!("{:02x} {:02x} 5c e1 76 60 9b 00", (priority >> 8) & 0xff, priority & 0xff)
}

pub fn iface_up(iface: &MockInterface, elapsed: f64) -> bool {
    // An error-disabled port is held down by the switch regardless of anything
    // else.
    if iface.err_disable_cause.is_some() {
        return false;
    }
    if iface.flaps {
        (elapsed.max(0.0) as u64 / FLAP_HALF_PERIOD_SECS) % 2 == 0
    } else {
        iface.up
    }
}

// Deterministic per-(device, interface, kind) rate so counters differ but
// never regress.
fn counter(dev_idx: usize, ifindex: i64, kind: u64, elapsed: f64) -> u64 {
    let seed = dev_idx as u64 * 131 + ifindex as u64 * 17 + kind * 7;
    let base = seed * 1_000_003;
    let rate = 5_000 + (seed * 37) % 45_000; // bytes or packets per second
    base + (rate as f64 * elapsed.max(0.0)) as u64
}

// A couple of ports accumulate errors slowly; everything else stays clean.
fn error_counter(dev_idx: usize, ifindex: i64, kind: u64, elapsed: f64) -> u64 {
    if (dev_idx as u64 + ifindex as u64 + kind) % 11 == 3 {
        (elapsed.max(0.0) as u64) / 60
    } else {
        0
    }
}

// Simulated-fault counter: rate/sec accumulated over elapsed (monotonic, so
// the poller's validate_counters accepts it). 0 when the port is clean.
fn fault_counter(rate: u64, elapsed: f64) -> u64 {
    (rate as f64 * elapsed.max(0.0)) as u64
}

// Octets for a saturated port: ~95% of line rate. bytes/s = speed_mbps*1e6/8.
// The +kind offset keeps rx/tx distinct and gives a non-zero base.
fn saturated_octets(speed_mbps: u64, kind: u64, elapsed: f64) -> u64 {
    let bytes_per_sec = (speed_mbps as f64 * 1_000_000.0 / 8.0) * 0.95;
    kind * 1_000 + (bytes_per_sec * elapsed.max(0.0)) as u64
}

// ifHighSpeed for a renegotiating port: full rate, then 1/10th, repeating —
// so the health store records speed-change events. Clean ports return their
// static speed.
fn reported_speed(iface: &MockInterface, elapsed: f64) -> u64 {
    if iface.renegotiates && (elapsed.max(0.0) as u64 / RENEG_HALF_PERIOD_SECS) % 2 == 1 {
        (iface.speed_mbps / 10).max(10)
    } else {
        iface.speed_mbps
    }
}

// Milli-celsius, bounded to roughly 39..45 °C.
pub fn sensor_value_milli(dev_idx: usize, sensor_idx: i64, elapsed: f64) -> u64 {
    let phase = dev_idx as f64 * 1.3 + sensor_idx as f64 * 0.7;
    (42_000.0 + 3_000.0 * (elapsed / 47.0 + phase).sin()) as u64
}

// ---------------------------------------------------------------------------
// Table / object generation
// ---------------------------------------------------------------------------

fn entry(index: serde_json::Value, objects: serde_json::Value) -> SNMPBotResultEntry {
    serde_json::from_value(json!({"HostID": "mock", "Index": index, "Objects": objects}))
        .expect("mock entry must match SNMPBotResultEntry shape")
}

fn response(id: &str, entries: Vec<SNMPBotResultEntry>) -> SNMPBotResponse {
    SNMPBotResponse {
        i_d: id.to_string(),
        index_keys: Vec::new(),
        object_keys: Vec::new(),
        entries,
    }
}

impl Topology {
    fn device_by_fqdn(&self, fqdn: &str) -> Option<(usize, &MockDevice)> {
        let name = fqdn.strip_suffix(&format!(".{}", DOMAIN))?;
        self.devices.iter().enumerate().find(|(_, d)| d.name == name)
    }

    // LLDP neighbors of `dev`: its own declared peers plus links declared on
    // the remote side pointing back at it. (local ifindex, remote device idx,
    // remote interface name)
    fn neighbors(&self, dev: &MockDevice) -> Vec<(i64, usize, &'static str)> {
        let mut out: Vec<(i64, usize, &'static str)> = Vec::new();
        for iface in dev.interfaces.iter() {
            if let Some((peer_name, peer_iface)) = iface.peer {
                if let Some((peer_idx, _)) = self.devices.iter().enumerate().find(|(_, d)| d.name == peer_name) {
                    out.push((iface.ifindex, peer_idx, peer_iface));
                }
            }
        }
        for (other_idx, other) in self.devices.iter().enumerate() {
            if other.name == dev.name {
                continue;
            }
            for other_iface in other.interfaces.iter() {
                if let Some((peer_name, peer_iface)) = other_iface.peer {
                    if peer_name == dev.name {
                        if let Some(local) = dev.interfaces.iter().find(|i| i.name == peer_iface) {
                            if !out.iter().any(|(ifidx, oidx, _)| *ifidx == local.ifindex && *oidx == other_idx) {
                                out.push((local.ifindex, other_idx, other_iface.name));
                            }
                        }
                    }
                }
            }
        }
        out
    }

    // STP member ports of a vlan: every peered interface of the device —
    // except that LACP-bundled members are represented by their port-channel,
    // like real switches running STP on the aggregate. Members with defaulted
    // LACP stay individual STP ports (their bundle never formed), which keeps
    // a-02's blocked backup uplink visible in the trees.
    fn stp_ports(dev: &MockDevice) -> Vec<(i64, &MockInterface)> {
        let bundled = |ifindex: i64| {
            dev.lags.iter().any(|lag| lag.members.contains(&ifindex) && !lag.defaulted_members.contains(&ifindex))
        };
        let formed_lag = |ifindex: i64| {
            dev.lags.iter().any(|lag| lag.ifindex == ifindex && lag.members.iter().any(|m| !lag.defaulted_members.contains(m)))
        };
        dev.interfaces
            .iter()
            .filter(|i| (i.peer.is_some() && !bundled(i.ifindex)) || formed_lag(i.ifindex))
            .enumerate()
            .map(|(pos, iface)| (pos as i64 + 1, iface))
            .collect()
    }

    // Q-BRIDGE bridge ports: every interface, numbered by position. Distinct
    // from stp_ports (peered only) — VLAN membership covers access ports too.
    fn bridge_ports(dev: &MockDevice) -> Vec<(i64, &MockInterface)> {
        dev.interfaces
            .iter()
            .enumerate()
            .map(|(pos, iface)| (pos as i64 + 1, iface))
            .collect()
    }

    pub fn table(&self, fqdn: &str, vlan: Option<i64>, table_id: &str, elapsed: f64) -> Option<SNMPBotResponse> {
        let (dev_idx, dev) = self.device_by_fqdn(fqdn)?;
        match table_id {
            "IF-MIB::ifTable" => {
                let entries = dev.interfaces.iter().map(|iface| {
                    entry(
                        json!({"IF-MIB::ifIndex": iface.ifindex}),
                        json!({
                            "IF-MIB::ifIndex": iface.ifindex,
                            "IF-MIB::ifDescr": iface.descr,
                            "IF-MIB::ifType": if dev.lags.iter().any(|l| l.ifindex == iface.ifindex) { "ieee8023adLag" } else { "ethernetCsmacd" },
                            "IF-MIB::ifPhysAddress": iface_mac(dev_idx, iface.ifindex),
                            // Ports are configured no-shut; a down oper state is
                            // a link fault (or err-disable), not an admin shut.
                            "IF-MIB::ifAdminStatus": "up",
                            "IF-MIB::ifOperStatus": if iface_up(iface, elapsed) { "up" } else { "down" },
                            "IF-MIB::ifInErrors": error_counter(dev_idx, iface.ifindex, 1, elapsed) + fault_counter(iface.error_rate, elapsed),
                            "IF-MIB::ifOutErrors": error_counter(dev_idx, iface.ifindex, 2, elapsed),
                            "IF-MIB::ifOutDiscards": error_counter(dev_idx, iface.ifindex, 3, elapsed) + fault_counter(iface.discard_rate, elapsed),
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "IF-MIB::ifXTable" => {
                let entries = dev.interfaces.iter().map(|iface| {
                    let c = |kind: u64| counter(dev_idx, iface.ifindex, kind, elapsed);
                    // Saturated ports drive octets at ~95% of line rate so the
                    // health store's utilization signal trips; others stay quiet.
                    let octets = |kind: u64| if iface.saturated { saturated_octets(iface.speed_mbps, kind, elapsed) } else { c(kind) };
                    entry(
                        json!({"IF-MIB::ifIndex": iface.ifindex}),
                        json!({
                            "IF-MIB::ifName": iface.name,
                            "IF-MIB::ifAlias": iface.alias,
                            "IF-MIB::ifHighSpeed": reported_speed(iface, elapsed),
                            "IF-MIB::ifHCInOctets": octets(10),
                            "IF-MIB::ifHCOutOctets": octets(11),
                            "IF-MIB::ifHCInUcastPkts": c(12),
                            "IF-MIB::ifHCInMulticastPkts": c(13),
                            "IF-MIB::ifHCInBroadcastPkts": c(14),
                            "IF-MIB::ifHCOutUcastPkts": c(15),
                            "IF-MIB::ifHCOutMulticastPkts": c(16),
                            "IF-MIB::ifHCOutBroadcastPkts": c(17),
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "ENTITY-MIB::entPhysicalTable" => {
                // Chassis entry (discovery: device_type + software_version)...
                let mut entries = vec![entry(
                    json!({"ENTITY-MIB::entPhysicalIndex": 1}),
                    json!({
                        "ENTITY-MIB::entPhysicalClass": "chassis",
                        "ENTITY-MIB::entPhysicalDescr": dev.sys_descr,
                        "ENTITY-MIB::entPhysicalModelName": dev.model,
                        "ENTITY-MIB::entPhysicalSoftwareRev": dev.sw_rev,
                    }),
                )];
                // ...plus sensor identities (entitypoller). Sensor names lead
                // with the interface name so the sensor-to-interface
                // association resolves.
                for (sensor_idx, iface) in dev.interfaces.iter().filter(|i| i.peer.is_some()).enumerate() {
                    entries.push(entry(
                        json!({"ENTITY-MIB::entPhysicalIndex": 1001 + sensor_idx as i64}),
                        json!({
                            "ENTITY-MIB::entPhysicalName": format!("{} Temperature Sensor", iface.name),
                            "ENTITY-MIB::entPhysicalDescr": "Transceiver temperature sensor",
                        }),
                    ));
                }
                entries.push(entry(
                    json!({"ENTITY-MIB::entPhysicalIndex": 2001}),
                    json!({
                        "ENTITY-MIB::entPhysicalName": "PSU 1",
                        "ENTITY-MIB::entPhysicalDescr": "Power supply sensor",
                    }),
                ));
                // Per-port physical entities for media/form-factor classification
                // (collectors::entity_media). Modeled on real Cisco IOS-XE shapes
                // (verified on a C9300): fixed copper ports are class "port" held
                // by the fixed module; 10G uplinks are SFP cages (class
                // "container"), and a plugged optic is a class "port" *inside* the
                // container whose descr is the optic media. Every third cage is
                // left empty, and one populated cage carries a 1G optic in a 10G
                // slot so the SFP-vs-SFP+ split is exercised. Port-channels are
                // logical: no physical entity.
                let fixed_module = 3000i64;
                entries.push(entry(
                    json!({"ENTITY-MIB::entPhysicalIndex": fixed_module}),
                    json!({
                        "ENTITY-MIB::entPhysicalClass": "module",
                        "ENTITY-MIB::entPhysicalName": "Fixed Module 0",
                        "ENTITY-MIB::entPhysicalDescr": format!("{} - Fixed Module 0", dev.model),
                    }),
                ));
                let mut ent_idx = 3001i64;
                for iface in dev.interfaces.iter() {
                    if iface.name.starts_with("Po") {
                        continue;
                    }
                    if iface.speed_mbps >= 10000 {
                        let container = ent_idx;
                        ent_idx += 1;
                        entries.push(entry(
                            json!({"ENTITY-MIB::entPhysicalIndex": container}),
                            json!({
                                "ENTITY-MIB::entPhysicalClass": "container",
                                "ENTITY-MIB::entPhysicalName": format!("{} Container", iface.name),
                                "ENTITY-MIB::entPhysicalDescr": format!("{} Container", iface.name),
                            }),
                        ));
                        if iface.ifindex % 3 != 0 {
                            // Most optics are 10G (SFP+); every fifth is a 1G SFP
                            // seated in the 10G cage.
                            let optic = if iface.ifindex % 5 == 0 { "1000BaseLX SFP" } else { "SFP-10GBase-SR" };
                            let optic_port = ent_idx;
                            ent_idx += 1;
                            entries.push(entry(
                                json!({"ENTITY-MIB::entPhysicalIndex": optic_port}),
                                json!({
                                    "ENTITY-MIB::entPhysicalClass": "port",
                                    "ENTITY-MIB::entPhysicalName": iface.name,
                                    "ENTITY-MIB::entPhysicalDescr": optic,
                                    "ENTITY-MIB::entPhysicalContainedIn": container,
                                }),
                            ));
                        }
                    } else {
                        entries.push(entry(
                            json!({"ENTITY-MIB::entPhysicalIndex": ent_idx}),
                            json!({
                                "ENTITY-MIB::entPhysicalClass": "port",
                                "ENTITY-MIB::entPhysicalName": iface.name,
                                "ENTITY-MIB::entPhysicalDescr": iface.descr,
                                "ENTITY-MIB::entPhysicalContainedIn": fixed_module,
                            }),
                        ));
                        ent_idx += 1;
                    }
                }
                Some(response(table_id, entries))
            }
            "ENTITY-SENSOR-MIB::entPhySensorTable" | "CISCO-ENTITY-SENSOR-MIB::entSensorValueTable" => {
                let style = if table_id.starts_with("CISCO") { SensorStyle::Cisco } else { SensorStyle::Standard };
                if dev.sensor_style != style {
                    return None;
                }
                let (value_key, scale_key, precision_key, type_key) = if style == SensorStyle::Cisco {
                    ("CISCO-ENTITY-SENSOR-MIB::entSensorValue", "CISCO-ENTITY-SENSOR-MIB::entSensorScale",
                     "CISCO-ENTITY-SENSOR-MIB::entSensorPrecision", "CISCO-ENTITY-SENSOR-MIB::entSensorType")
                } else {
                    ("ENTITY-SENSOR-MIB::entPhySensorValue", "ENTITY-SENSOR-MIB::entPhySensorScale",
                     "ENTITY-SENSOR-MIB::entPhySensorPrecision", "ENTITY-SENSOR-MIB::entPhySensorType")
                };
                let uplink_count = dev.interfaces.iter().filter(|i| i.peer.is_some()).count() as i64;
                let mut entries = Vec::new();
                for sensor_idx in 0..uplink_count {
                    entries.push(entry(
                        json!({"ENTITY-MIB::entPhysicalIndex": 1001 + sensor_idx}),
                        json!({
                            value_key: sensor_value_milli(dev_idx, sensor_idx, elapsed),
                            scale_key: "milli",
                            precision_key: 0,
                            type_key: "celsius",
                        }),
                    ));
                }
                entries.push(entry(
                    json!({"ENTITY-MIB::entPhysicalIndex": 2001}),
                    json!({
                        value_key: 12_100 + (dev_idx as u64 * 13) % 200,
                        scale_key: "milli",
                        precision_key: 0,
                        type_key: "voltsDC",
                    }),
                ));
                Some(response(table_id, entries))
            }
            "CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleTable" => {
                if dev.stp_style != StpStyle::Cisco || dev.stp_vlans.is_empty() {
                    return None;
                }
                let mut entries = Vec::new();
                for vlan in dev.stp_vlans.iter() {
                    for (bridge_port, iface) in Self::stp_ports(dev) {
                        // VLAN 30 split-brain demo: dist1 refuses to defer, so
                        // its uplink is designated (not root) — it self-roots
                        // alongside core1, which it is directly linked to.
                        let role = if dev.name == "dist1" && *vlan == 30 {
                            "designated"
                        } else {
                            stp_role(dev, iface)
                        };
                        entries.push(entry(
                            json!({
                                "CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleInstanceIndex": vlan,
                                "CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRolePortIndex": bridge_port,
                            }),
                            json!({"CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleValue": role}),
                        ));
                    }
                }
                Some(response(table_id, entries))
            }
            "BRIDGE-MIB::dot1dBasePortTable" => {
                // Per-VLAN community form (entitypoller STP): STP member ports
                // only. Base community form (vlanpoller Q-BRIDGE fallback):
                // every bridge port, answered only by QBridge-style devices.
                let ports = match vlan {
                    Some(vlan) => {
                        if dev.stp_style != StpStyle::Cisco || !dev.stp_vlans.contains(&vlan) {
                            return None;
                        }
                        Self::stp_ports(dev)
                    }
                    None => {
                        if dev.vlan_style != VlanStyle::QBridge {
                            return None;
                        }
                        Self::bridge_ports(dev)
                    }
                };
                let entries = ports.into_iter().map(|(bridge_port, iface)| {
                    entry(
                        json!({"BRIDGE-MIB::dot1dBasePort": bridge_port}),
                        json!({"BRIDGE-MIB::dot1dBasePortIfIndex": iface.ifindex}),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "CISCO-VTP-MIB::jaspyVlanTrunkPortTable" => {
                if dev.vlan_style != VlanStyle::Cisco {
                    return None;
                }
                // Like real Cisco gear with the default "allowed vlan all"
                // config: the enabled bitmaps report every VLAN, and the
                // collector must intersect with vtpVlanTable to get the truth.
                let all_1k = hex_bitmap(&(1..1024).collect::<Vec<i64>>(), 0, 128);
                let all_2k: String = hex_bitmap(&(1024..2048).collect::<Vec<i64>>(), 1024, 128);
                let entries = dev.interfaces.iter().map(|iface| {
                    let trunking = if is_trunk(dev, iface) { "trunking" } else { "notTrunking" };
                    entry(
                        json!({"CISCO-VTP-MIB::vlanTrunkPortIfIndex": iface.ifindex}),
                        json!({
                            "CISCO-VTP-MIB::vlanTrunkPortDynamicStatus": trunking,
                            "CISCO-VTP-MIB::vlanTrunkPortNativeVlan": TRUNK_NATIVE_VLAN,
                            "CISCO-VTP-MIB::vlanTrunkPortVlansEnabled": all_1k,
                            "CISCO-VTP-MIB::vlanTrunkPortVlansEnabled2k": all_2k,
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "CISCO-VTP-MIB::vtpVlanTable" => {
                if dev.vlan_style != VlanStyle::Cisco {
                    return None;
                }
                let entries = dev.vlans.iter().map(|vlan| {
                    entry(
                        json!({"CISCO-VTP-MIB::managementDomainIndex": 1, "CISCO-VTP-MIB::vtpVlanIndex": vlan}),
                        json!({
                            "CISCO-VTP-MIB::vtpVlanState": "operational",
                            "CISCO-VTP-MIB::vtpVlanType": "ethernet",
                            "CISCO-VTP-MIB::vtpVlanName": format!("mock-vlan-{}", vlan),
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "CISCO-VLAN-MEMBERSHIP-MIB::vmMembershipTable" => {
                if dev.vlan_style != VlanStyle::Cisco {
                    return None;
                }
                let entries = dev.interfaces.iter().filter(|i| !is_trunk(dev, i)).map(|iface| {
                    entry(
                        json!({"IF-MIB::ifIndex": iface.ifindex}),
                        json!({
                            "CISCO-VLAN-MEMBERSHIP-MIB::vmVlanType": "static",
                            "CISCO-VLAN-MEMBERSHIP-MIB::vmVlan": access_vlan(iface),
                            "CISCO-VLAN-MEMBERSHIP-MIB::vmPortStatus": "active",
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "CISCO-ERR-DISABLE-MIB::cErrDisableIfStatusTable" => {
                // One row per error-disabled port (the table is empty on a
                // healthy switch). The cause is the CISCO-ERR-DISABLE-MIB enum
                // name, exactly as snmpbot renders an ENUM column.
                let entries = dev.interfaces.iter().filter_map(|iface| {
                    iface.err_disable_cause.map(|cause| entry(
                        json!({
                            "IF-MIB::ifIndex": iface.ifindex,
                            "CISCO-ERR-DISABLE-MIB::cErrDisableIfStatusVlanIndex": 0,
                        }),
                        json!({
                            "CISCO-ERR-DISABLE-MIB::cErrDisableIfStatusCause": cause,
                            "CISCO-ERR-DISABLE-MIB::cErrDisableIfStatusTimeToRecover": 39,
                        }),
                    ))
                }).collect();
                Some(response(table_id, entries))
            }
            "Q-BRIDGE-MIB::dot1qPortVlanTable" => {
                if dev.vlan_style != VlanStyle::QBridge {
                    return None;
                }
                let entries = Self::bridge_ports(dev).into_iter().map(|(bridge_port, iface)| {
                    let pvid = if is_trunk(dev, iface) { TRUNK_NATIVE_VLAN } else { access_vlan(iface) };
                    entry(
                        json!({"BRIDGE-MIB::dot1dBasePort": bridge_port}),
                        json!({"Q-BRIDGE-MIB::dot1qPvid": pvid}),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "Q-BRIDGE-MIB::dot1qVlanCurrentTable" | "Q-BRIDGE-MIB::dot1qVlanStaticTable" => {
                if dev.vlan_style != VlanStyle::QBridge {
                    return None;
                }
                let is_static = table_id.contains("Static");
                // Per VLAN: trunks carry every VLAN (untagged only on their
                // native), access ports appear untagged on their own VLAN.
                // The Static variant carries the same membership plus the
                // VLAN name (its only source in Q-BRIDGE-MIB) and has no
                // TimeMark in its index.
                let entries = dev.vlans.iter().map(|vlan| {
                    let mut egress: Vec<i64> = Vec::new();
                    let mut untagged: Vec<i64> = Vec::new();
                    for (bridge_port, iface) in Self::bridge_ports(dev) {
                        if is_trunk(dev, iface) {
                            egress.push(bridge_port);
                            if *vlan == TRUNK_NATIVE_VLAN {
                                untagged.push(bridge_port);
                            }
                        } else if access_vlan(iface) == *vlan {
                            egress.push(bridge_port);
                            untagged.push(bridge_port);
                        }
                    }
                    let len = (dev.interfaces.len() + 7) / 8;
                    if is_static {
                        // VLAN 20 is deliberately named differently from the
                        // Cisco devices' vtpVlanName so the VLANs page's
                        // name-mismatch warning has something to show.
                        let name = if *vlan == 20 { "guest-legacy".to_string() } else { format!("mock-vlan-{}", vlan) };
                        entry(
                            json!({"Q-BRIDGE-MIB::dot1qVlanIndex": vlan}),
                            json!({
                                "Q-BRIDGE-MIB::dot1qVlanStaticName": name,
                                "Q-BRIDGE-MIB::dot1qVlanStaticEgressPorts": hex_bitmap(&egress, 1, len),
                                "Q-BRIDGE-MIB::dot1qVlanStaticUntaggedPorts": hex_bitmap(&untagged, 1, len),
                            }),
                        )
                    } else {
                        entry(
                            json!({"Q-BRIDGE-MIB::dot1qVlanTimeMark": 0, "Q-BRIDGE-MIB::dot1qVlanIndex": vlan}),
                            json!({
                                "Q-BRIDGE-MIB::dot1qVlanCurrentEgressPorts": hex_bitmap(&egress, 1, len),
                                "Q-BRIDGE-MIB::dot1qVlanCurrentUntaggedPorts": hex_bitmap(&untagged, 1, len),
                            }),
                        )
                    }
                }).collect();
                Some(response(table_id, entries))
            }
            "BRIDGE-MIB::dot1dStpPortTable" => {
                let vlan = vlan?;
                if dev.stp_style != StpStyle::Cisco || !dev.stp_vlans.contains(&vlan) {
                    return None;
                }
                let entries = Self::stp_ports(dev).into_iter().map(|(bridge_port, iface)| {
                    let role = stp_role(dev, iface);
                    entry(
                        json!({"BRIDGE-MIB::dot1dStpPort": bridge_port}),
                        json!({
                            "BRIDGE-MIB::dot1dStpPortDesignatedCost": if dev.name == "core1" { 0 } else { 4 },
                            "BRIDGE-MIB::dot1dStpPortPathCost": if role == "root" { 4 } else { 19 },
                            "BRIDGE-MIB::dot1dStpPortPriority": 128,
                            "BRIDGE-MIB::dot1dStpPortForwardTransitions": 1,
                            "BRIDGE-MIB::dot1dStpPortEnable": "enabled",
                            "BRIDGE-MIB::dot1dStpPortState": if role == "alternate" { "blocking" } else { "forwarding" },
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "BRIDGE-MIB::jaspyStpBridgeTable" => {
                // One-row jaspy view over the five per-vlan dot1dStp scalars
                // (the entitypoller's bridge polling); values delegate to
                // object() so the two addressing forms always agree.
                let objects: serde_json::Map<String, serde_json::Value> = [
                    "BRIDGE-MIB::dot1dStpTimeSinceTopologyChange",
                    "BRIDGE-MIB::dot1dStpTopChanges",
                    "BRIDGE-MIB::dot1dStpDesignatedRoot",
                    "BRIDGE-MIB::dot1dStpRootCost",
                    "BRIDGE-MIB::dot1dStpRootPort",
                ]
                .iter()
                .filter_map(|id| self.object(fqdn, vlan, id, elapsed).map(|value| (id.to_string(), value)))
                .collect();
                if objects.is_empty() {
                    return None;
                }
                Some(response(table_id, vec![entry(json!({"BRIDGE-MIB::jaspyStpBridgeInstance": 0}), json!(objects))]))
            }
            "HP-ICF-RPVST-MIB::jaspyRpvstPortVlanRoleTable"
            | "HP-ICF-RPVST-MIB::jaspyRpvstPortVlanStateTable"
            | "HP-ICF-RPVST-MIB::jaspyRpvstPortVlanCostTable" => {
                // HP RPVST+ single-column views on the base community (no
                // vlan host indexing); port index == ifIndex like ProCurve.
                if dev.stp_style != StpStyle::Rpvst {
                    return None;
                }
                let mut entries = Vec::new();
                for vlan in dev.stp_vlans.iter() {
                    for (_, iface) in Self::stp_ports(dev) {
                        let role = stp_role(dev, iface);
                        let index = json!({
                            "HP-ICF-RPVST-MIB::hpicfRpvstVlanId": vlan,
                            "HP-ICF-RPVST-MIB::hpicfRpvstPortIndex": iface.ifindex,
                        });
                        let objects = match table_id {
                            "HP-ICF-RPVST-MIB::jaspyRpvstPortVlanRoleTable" =>
                                json!({"HP-ICF-RPVST-MIB::hpicfRpvstPortVlanRole": role}),
                            "HP-ICF-RPVST-MIB::jaspyRpvstPortVlanStateTable" =>
                                json!({"HP-ICF-RPVST-MIB::hpicfRpvstPortVlanState": if role == "alternate" { "blocking" } else { "forwarding" }}),
                            _ =>
                                json!({"HP-ICF-RPVST-MIB::hpicfRpvstPortVlanPathCost": if role == "root" { 4 } else { 19 }}),
                        };
                        entries.push(entry(index, objects));
                    }
                }
                Some(response(table_id, entries))
            }
            "HP-ICF-RPVST-MIB::hpicfRpvstVlanTable" => {
                if dev.stp_style != StpStyle::Rpvst {
                    return None;
                }
                let root_port = Self::stp_ports(dev)
                    .into_iter()
                    .find(|(_, iface)| stp_role(dev, iface) == "root")
                    .map(|(_, iface)| iface.ifindex)
                    .unwrap_or(0);
                let entries = dev.stp_vlans.iter().map(|vlan| {
                    entry(
                        json!({"HP-ICF-RPVST-MIB::hpicfRpvstVlanId": vlan}),
                        json!({
                            "HP-ICF-RPVST-MIB::hpicfRpvstVlanRootPriority": 32768,
                            "HP-ICF-RPVST-MIB::hpicfRpvstVlanRootPort": root_port,
                            "HP-ICF-RPVST-MIB::hpicfRpvstVlanRootPathCost": 8,
                            // The Cisco core is the root here too.
                            "HP-ICF-RPVST-MIB::hpicfRpvstVlanRootMacAddress": base_mac_spaced(0),
                            // TimeTicks: seconds, like real snmpbot.
                            "HP-ICF-RPVST-MIB::hpicfRpvstVlanTimeSinceLastTopoChange": elapsed.max(0.0) as u64 + 3600,
                            "HP-ICF-RPVST-MIB::hpicfVlanTopoChangeCount": dev.stp_vlans.len() as i64 + vlan,
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "LLDP-MIB::lldpLocPortTable" => {
                // FortiOS-style firewalls leave lldpLocalSystemData empty: the
                // table exists but walks to nothing. Discovery must still map
                // this device's rem-table neighbors via the ifIndex fallback.
                if !dev.exposes_lldp_local {
                    return Some(response(table_id, Vec::new()));
                }
                let entries = dev.interfaces.iter().map(|iface| {
                    entry(
                        json!({"LLDP-MIB::lldpLocPortNum": iface.ifindex}),
                        json!({
                            "LLDP-MIB::lldpLocPortIdSubtype": "interfaceName",
                            "LLDP-MIB::lldpLocPortId": iface.name,
                            "LLDP-MIB::lldpLocPortDesc": iface.descr,
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "LLDP-MIB::lldpRemTable" => {
                // LLDP caches survive short link flaps, so adjacency is
                // emitted unconditionally: discovery resolves links even when
                // the flapping uplink happens to be down.
                let entries = self.neighbors(dev).into_iter().map(|(local_ifindex, peer_idx, peer_iface)| {
                    entry(
                        json!({"LLDP-MIB::lldpRemLocalPortNum": local_ifindex}),
                        json!({
                            "LLDP-MIB::lldpRemSysName": self.devices[peer_idx].name,
                            "LLDP-MIB::lldpRemChassisId": base_mac_spaced(peer_idx),
                            "LLDP-MIB::lldpRemPortId": peer_iface,
                            "LLDP-MIB::lldpRemPortIdSubtype": "interfaceName",
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "CISCO-CDP-MIB::cdpCacheTable" => {
                // Access switches see an IP phone on some live desk ports (the
                // odd ifIndexes); the even ones model plain PCs that announce no
                // CDP/LLDP. The phones are off-fleet (not crawled devices), so
                // they never resolve to a monitored link — they exercise the
                // "CDP neighbor as plain text" path on the device page, and the
                // phone/PC split lets the issues page demonstrate that
                // interface-health warnings are suppressed on neighbour-less
                // access ports but kept on neighboured ones.
                let entries = if dev.name.starts_with("access-") {
                    dev.interfaces.iter()
                        .filter(|iface| {
                            iface.peer.is_none()
                                && iface.speed_mbps < 10000
                                && !iface.name.starts_with("Po")
                                && !dev.lags.iter().any(|lag| lag.members.contains(&iface.ifindex))
                                && iface.ifindex % 2 == 1
                                && iface_up(iface, elapsed)
                        })
                        .map(|iface| entry(
                            json!({
                                "CISCO-CDP-MIB::cdpCacheIfIndex": iface.ifindex,
                                "CISCO-CDP-MIB::cdpCacheDeviceIndex": 1,
                            }),
                            json!({
                                "CISCO-CDP-MIB::cdpCacheDeviceId": format!("SEP{:012X}", 0x0011_0000_0000u64 + iface.ifindex as u64),
                                "CISCO-CDP-MIB::cdpCacheDevicePort": "Port 1",
                            }),
                        ))
                        .collect()
                } else {
                    Vec::new()
                };
                Some(response(table_id, entries))
            }
            // LAG tables for the lagpoller. Cisco-style devices answer the
            // PAgP table with a row per physical port, all groups 0 — the
            // live C2960CX shape: pagpGroupIfIndex reports nothing for LACP
            // bundles, which live only in the dot3ad tables. Others answer
            // valid-but-empty to keep the logs quiet.
            "CISCO-PAGP-MIB::pagpPortTable" => {
                if dev.vlan_style != VlanStyle::Cisco {
                    return Some(response(table_id, Vec::new()));
                }
                let entries = dev.interfaces.iter()
                    .filter(|iface| !dev.lags.iter().any(|lag| lag.ifindex == iface.ifindex))
                    .map(|iface| {
                        entry(
                            json!({"IF-MIB::ifIndex": iface.ifindex}),
                            json!({"CISCO-PAGP-MIB::pagpEthcOperationMode": 1, "CISCO-PAGP-MIB::pagpGroupIfIndex": 0}),
                        )
                    })
                    .collect();
                Some(response(table_id, entries))
            }
            "IEEE8023-LAG-MIB::dot3adAggTable" => {
                let entries = dev.lags.iter().map(|lag| {
                    let partner = lag.members.iter()
                        .find(|m| !lag.defaulted_members.contains(m) && !lag.down_members.contains(m))
                        .map(|m| self.lag_partner_mac(dev, lag, *m))
                        .unwrap_or_else(|| "00:00:00:00:00:00".to_string());
                    entry(
                        json!({"IEEE8023-LAG-MIB::dot3adAggIndex": lag.ifindex}),
                        json!({
                            "IEEE8023-LAG-MIB::dot3adAggActorAdminKey": lag.ifindex - 5000,
                            "IEEE8023-LAG-MIB::dot3adAggActorSystemID": base_mac(dev_idx),
                            "IEEE8023-LAG-MIB::dot3adAggPartnerSystemID": partner,
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "IEEE8023-LAG-MIB::dot3adAggPortTable" => {
                let mut entries = Vec::new();
                for lag in dev.lags.iter() {
                    for (pos, member) in lag.members.iter().enumerate() {
                        let defaulted = lag.defaulted_members.contains(member);
                        let down = lag.down_members.contains(member);
                        // A down member is still a configured member (its admin
                        // key matches the aggregate, so the collector re-adds it)
                        // but detached with an empty actor state — it cannot
                        // bundle while its link is down.
                        let actor_state = if down {
                            json!([])
                        } else if defaulted {
                            json!(["lacpActivity", "aggregation", "defaulted"])
                        } else {
                            json!(["lacpActivity", "aggregation", "synchronization", "collecting", "distributing"])
                        };
                        let inactive = defaulted || down;
                        entries.push(entry(
                            json!({"IEEE8023-LAG-MIB::dot3adAggPortIndex": member}),
                            json!({
                                "IEEE8023-LAG-MIB::dot3adAggPortActorAdminKey": lag.ifindex - 5000,
                                "IEEE8023-LAG-MIB::dot3adAggPortPartnerOperSystemID":
                                    if inactive { "00:00:00:00:00:00".to_string() } else { self.lag_partner_mac(dev, lag, *member) },
                                "IEEE8023-LAG-MIB::dot3adAggPortAttachedAggID": if down { 0 } else { lag.ifindex },
                                "IEEE8023-LAG-MIB::dot3adAggPortPartnerOperPort": if inactive { 0 } else { pos as i64 + 1 },
                                "IEEE8023-LAG-MIB::dot3adAggPortActorOperState": actor_state,
                                "IEEE8023-LAG-MIB::dot3adAggPortPartnerOperState": if inactive { json!([]) } else { actor_state.clone() },
                            }),
                        ));
                    }
                }
                Some(response(table_id, entries))
            }
            // PoE (POWER-ETHERNET-MIB + Cisco ext). PoE-capable switches answer
            // with rows; others answer valid-but-empty (like a real switch that
            // lacks the MIB — an empty walk, not a 404).
            "POWER-ETHERNET-MIB::pethMainPseTable" => {
                let poe = match &dev.poe {
                    Some(p) => p,
                    None => return Some(response(table_id, Vec::new())),
                };
                let consumed_w: i64 = poe.delivering.iter().map(|(_, mw)| mw).sum::<i64>() / 1000;
                Some(response(table_id, vec![entry(
                    json!({"POWER-ETHERNET-MIB::pethMainPseGroupIndex": 1}),
                    json!({
                        "POWER-ETHERNET-MIB::pethMainPsePower": poe.budget_w,
                        "POWER-ETHERNET-MIB::pethMainPseOperStatus": "on",
                        "POWER-ETHERNET-MIB::pethMainPseConsumptionPower": consumed_w,
                        "POWER-ETHERNET-MIB::pethMainPseUsageThreshold": 0,
                    }),
                )]))
            }
            "POWER-ETHERNET-MIB::pethPsePortTable" => {
                let poe = match &dev.poe {
                    Some(p) => p,
                    None => return Some(response(table_id, Vec::new())),
                };
                let entries = poe_ports(dev).into_iter().enumerate().map(|(i, iface)| {
                    let delivering = poe.delivering.iter().any(|(ifx, _)| *ifx == iface.ifindex);
                    entry(
                        json!({"POWER-ETHERNET-MIB::pethPsePortGroupIndex": 1, "POWER-ETHERNET-MIB::pethPsePortIndex": i as i64 + 1}),
                        json!({
                            "POWER-ETHERNET-MIB::pethPsePortAdminEnable": "true",
                            "POWER-ETHERNET-MIB::pethPsePortDetectionStatus": if delivering { "deliveringPower" } else { "searching" },
                            "POWER-ETHERNET-MIB::pethPsePortPowerPriority": "low",
                            "POWER-ETHERNET-MIB::pethPsePortType": if delivering { "Ieee PD" } else { "" },
                            "POWER-ETHERNET-MIB::pethPsePortPowerClassifications": if delivering { "class4" } else { "class0" },
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            "CISCO-POWER-ETHERNET-EXT-MIB::cpeExtPsePortTable" => {
                let poe = match &dev.poe {
                    Some(p) => p,
                    None => return Some(response(table_id, Vec::new())),
                };
                let entries = poe_ports(dev).into_iter().enumerate().map(|(i, iface)| {
                    let mw = poe.delivering.iter().find(|(ifx, _)| *ifx == iface.ifindex).map(|(_, mw)| *mw).unwrap_or(0);
                    entry(
                        json!({"POWER-ETHERNET-MIB::pethPsePortGroupIndex": 1, "POWER-ETHERNET-MIB::pethPsePortIndex": i as i64 + 1}),
                        json!({
                            "CISCO-POWER-ETHERNET-EXT-MIB::cpeExtPsePortPwrAllocated": if mw > 0 { 15400 } else { 0 },
                            "CISCO-POWER-ETHERNET-EXT-MIB::cpeExtPsePortPwrConsumption": mw,
                            "CISCO-POWER-ETHERNET-EXT-MIB::cpeExtPsePortMaxPwrDrawn": mw + mw / 10,
                            "CISCO-POWER-ETHERNET-EXT-MIB::cpeExtPsePortEntPhyIndex": port_ent_index(dev, iface.ifindex).unwrap_or(0),
                        }),
                    )
                }).collect();
                Some(response(table_id, entries))
            }
            _ => None,
        }
    }

    // LACP partner system id for one bundle member: the topology peer's base
    // MAC when the member is a discovered link, else the lag's declared
    // partner (an unmonitored device).
    fn lag_partner_mac(&self, dev: &MockDevice, lag: &MockLag, member: i64) -> String {
        if let Some(iface) = dev.interfaces.iter().find(|i| i.ifindex == member) {
            if let Some((peer_name, _)) = iface.peer {
                if let Some((peer_idx, _)) = self.devices.iter().enumerate().find(|(_, d)| d.name == peer_name) {
                    return base_mac(peer_idx);
                }
            }
        }
        lag.partner_mac.to_string()
    }

    pub fn object(&self, fqdn: &str, vlan: Option<i64>, object_id: &str, elapsed: f64) -> Option<serde_json::Value> {
        let (dev_idx, dev) = self.device_by_fqdn(fqdn)?;
        // Per-VLAN bridge scalars (community@vlan@fqdn form), answered only
        // for VLANs the device runs STP on. core1 is always the root.
        if let Some(vlan) = vlan.filter(|v| dev.stp_style == StpStyle::Cisco && dev.stp_vlans.contains(v)) {
            // Tier by name: core 0, dist 4, access 8 — matches the per-port
            // path costs the STP tables report.
            // a-01 on VLAN 63 hears a superior root (priority 4096) from an
            // unmonitored bridge upstream — jaspy elects core1 for the VLAN, so
            // this reads as a root mismatch pinned (misleadingly) on a-01.
            let off_fleet = dev.name == "access-hall-a-01" && vlan == 63;
            // VLAN 30 split-brain: dist1 self-roots (reports its own bridge id,
            // cost 0, no root port) while core1 also roots the VLAN — and the
            // two are directly linked, so it is a genuine (critical) split brain.
            let self_root = dev.name == "dist1" && vlan == 30;
            let root_cost = if self_root { 0 } else if off_fleet { 60000 } else if dev.name == "core1" { 0 } else if dev.name.starts_with("dist") { 4 } else { 8 };
            return match object_id {
                "BRIDGE-MIB::dot1dStpDesignatedRoot" if off_fleet => Some(json!(off_fleet_root_bridge_id(vlan))),
                "BRIDGE-MIB::dot1dStpDesignatedRoot" if self_root => Some(json!(bridge_id_spaced(24576 + vlan, dev_idx))),
                "BRIDGE-MIB::dot1dStpDesignatedRoot" => Some(json!(bridge_id_spaced(24576 + vlan, 0))),
                "BRIDGE-MIB::dot1dStpRootCost" => Some(json!(root_cost)),
                "BRIDGE-MIB::dot1dStpRootPort" if self_root => Some(json!(0)),
                "BRIDGE-MIB::dot1dStpRootPort" => {
                    let root_port = Self::stp_ports(dev)
                        .into_iter()
                        .find(|(_, iface)| stp_role(dev, iface) == "root")
                        .map(|(bridge_port, _)| bridge_port)
                        .unwrap_or(0);
                    Some(json!(root_port))
                }
                "BRIDGE-MIB::dot1dStpTopChanges" => Some(json!(dev_idx as i64 * 3 + vlan)),
                // TimeTicks: snmpbot renders them as seconds (verified live).
                "BRIDGE-MIB::dot1dStpTimeSinceTopologyChange" => Some(json!(elapsed.max(0.0) as u64 + 3600)),
                _ => None,
            };
        }
        match object_id {
            "SNMPv2-MIB::sysDescr" => Some(json!(dev.sys_descr)),
            // A FortiOS-style firewall (exposes_lldp_local = false) is not a
            // bridge and publishes no LLDP local chassis id, so both are absent
            // and discovery leaves its base_mac null.
            "BRIDGE-MIB::dot1dBaseBridgeAddress" if dev.exposes_lldp_local => Some(json!(base_mac_spaced(dev_idx))),
            "LLDP-MIB::lldpLocChassisId" if dev.exposes_lldp_local => Some(json!(base_mac_spaced(dev_idx))),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::poller::SNMPBotResultEntryObjectValue;

    const ALL_TABLES: [&str; 23] = [
        "IF-MIB::ifTable",
        "IF-MIB::ifXTable",
        "ENTITY-MIB::entPhysicalTable",
        "ENTITY-SENSOR-MIB::entPhySensorTable",
        "CISCO-ENTITY-SENSOR-MIB::entSensorValueTable",
        "CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleTable",
        "LLDP-MIB::lldpLocPortTable",
        "LLDP-MIB::lldpRemTable",
        "CISCO-CDP-MIB::cdpCacheTable",
        "CISCO-VTP-MIB::jaspyVlanTrunkPortTable",
        "CISCO-VTP-MIB::vtpVlanTable",
        "CISCO-VLAN-MEMBERSHIP-MIB::vmMembershipTable",
        "Q-BRIDGE-MIB::dot1qPortVlanTable",
        "Q-BRIDGE-MIB::dot1qVlanCurrentTable",
        "Q-BRIDGE-MIB::dot1qVlanStaticTable",
        "HP-ICF-RPVST-MIB::jaspyRpvstPortVlanRoleTable",
        "HP-ICF-RPVST-MIB::jaspyRpvstPortVlanStateTable",
        "HP-ICF-RPVST-MIB::jaspyRpvstPortVlanCostTable",
        "HP-ICF-RPVST-MIB::hpicfRpvstVlanTable",
        "CISCO-PAGP-MIB::pagpPortTable",
        "IEEE8023-LAG-MIB::dot3adAggTable",
        "IEEE8023-LAG-MIB::dot3adAggPortTable",
        "CISCO-ERR-DISABLE-MIB::cErrDisableIfStatusTable",
    ];

    #[test]
    fn tables_serialize_with_snmpbot_shape() {
        let topo = build();
        for dev in topo.devices.iter() {
            for table in ALL_TABLES.iter() {
                if let Some(resp) = topo.table(&dev.fqdn(), Some(10), table, 30.0) {
                    let value = serde_json::to_value(&resp).unwrap();
                    assert!(value.get("ID").is_some(), "{} missing PascalCase ID", table);
                    assert!(value.get("Entries").is_some(), "{} missing Entries", table);
                    // Round-trip through the exact structs the collectors parse.
                    let parsed: crate::collectors::poller::SNMPBotResponse =
                        serde_json::from_value(value).unwrap();
                    for entry in parsed.entries.iter() {
                        assert!(!entry.index.is_empty(), "{} entry without Index", table);
                    }
                }
            }
        }
    }

    #[test]
    fn err_disable_table_reports_the_seeded_port_and_it_is_oper_down() {
        let topo = build();
        let fqdn = "access-hall-a-01.mock.jaspy";
        let resp = topo.table(fqdn, None, "CISCO-ERR-DISABLE-MIB::cErrDisableIfStatusTable", 30.0).unwrap();
        let value = serde_json::to_value(&resp).unwrap();
        let entries = value["Entries"].as_array().unwrap();
        // Exactly the one seeded err-disabled port (Gi1/0/3 = 10203).
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["Index"]["IF-MIB::ifIndex"].as_i64(), Some(10203));
        assert_eq!(
            entries[0]["Objects"]["CISCO-ERR-DISABLE-MIB::cErrDisableIfStatusCause"].as_str(),
            Some("bpduGuard")
        );
        // A switch that error-disables a port forces it oper-down.
        let iftable = serde_json::to_value(topo.table(fqdn, None, "IF-MIB::ifTable", 30.0).unwrap()).unwrap();
        let oper = iftable["Entries"].as_array().unwrap().iter()
            .find(|e| e["Index"]["IF-MIB::ifIndex"].as_i64() == Some(10203))
            .and_then(|e| e["Objects"]["IF-MIB::ifOperStatus"].as_str());
        assert_eq!(oper, Some("down"));
    }

    // Pull a numeric object for one ifIndex out of a table response by
    // navigating the serialized JSON (avoids depending on the entry structs).
    fn obj_u64(resp: &SNMPBotResponse, ifindex: i64, key: &str) -> u64 {
        let value = serde_json::to_value(resp).unwrap();
        for e in value["Entries"].as_array().unwrap() {
            if e["Index"]["IF-MIB::ifIndex"].as_i64() == Some(ifindex) {
                return e["Objects"][key].as_u64().expect("numeric object");
            }
        }
        panic!("ifindex {} not found", ifindex);
    }

    #[test]
    fn simulated_faults_emit_elevated_signals() {
        let topo = build();
        let elapsed = 300.0; // 5 minutes in

        // a-01: 10204 drops ~200 discards/s, 10205 takes ~5 input errors/s.
        let a01 = topo.table("access-hall-a-01.mock.jaspy", None, "IF-MIB::ifTable", elapsed).unwrap();
        assert!(obj_u64(&a01, 10204, "IF-MIB::ifOutDiscards") >= 200 * 300, "discard fault should accumulate");
        assert!(obj_u64(&a01, 10205, "IF-MIB::ifInErrors") >= 5 * 300, "error fault should accumulate");
        // A clean neighbour stays near zero (only the slow baseline).
        assert!(obj_u64(&a01, 10206, "IF-MIB::ifOutDiscards") < 1000, "clean port should stay quiet");

        // b-01: 10201 saturated (~95% of 1G), 10202 renegotiates speed.
        let b01_x = |el: f64| topo.table("access-hall-b-01.mock.jaspy", None, "IF-MIB::ifXTable", el).unwrap();
        let octets = obj_u64(&b01_x(10.0), 10201, "IF-MIB::ifHCInOctets");
        let full_line = (1_000_000_000.0 / 8.0 * 10.0) as u64; // bytes at line rate over 10s
        assert!(octets > full_line / 2, "saturated port octets too low: {} vs {}", octets, full_line);
        let speed_full = obj_u64(&b01_x(0.0), 10202, "IF-MIB::ifHighSpeed");
        let speed_reneg = obj_u64(&b01_x(RENEG_HALF_PERIOD_SECS as f64), 10202, "IF-MIB::ifHighSpeed");
        assert_ne!(speed_full, speed_reneg, "renegotiating port should change speed over time");
    }

    #[test]
    fn lag_tables_decode_through_the_lagpoller() {
        use crate::collectors::lagpoller;
        let topo = build();
        let decode = |fqdn: &str| {
            let pagp = topo.table(fqdn, None, "CISCO-PAGP-MIB::pagpPortTable", 0.0).unwrap();
            let agg = topo.table(fqdn, None, "IEEE8023-LAG-MIB::dot3adAggTable", 0.0).unwrap();
            let ports = topo.table(fqdn, None, "IEEE8023-LAG-MIB::dot3adAggPortTable", 0.0).unwrap();
            let (groups, modes) = lagpoller::decode_pagp(&pagp);
            lagpoller::merge_cisco(lagpoller::decode_dot3ad(Some(&agg), Some(&ports)), groups, &modes)
        };

        // a-01: healthy 2-member LACP bundle to an unmonitored server.
        let lags = decode("access-hall-a-01.mock.jaspy");
        let group = &lags.groups[&5001];
        assert_eq!(group.protocol, "lacp");
        assert_eq!(group.members.len(), 2);
        assert!(group.members.values().all(|m| lagpoller::lacp_bundled(&m.actor_state)));
        let partners: std::collections::BTreeSet<_> = group.members.values().filter_map(|m| m.partner_system_id.clone()).collect();
        assert_eq!(partners.len(), 1, "healthy bundle agrees on one partner");

        // a-02: the broken demo — one member defaulted, the other bundled to
        // a different device than the defaulted one is wired to.
        let lags = decode("access-hall-a-02.mock.jaspy");
        let group = &lags.groups[&5001];
        assert_eq!(group.members.len(), 2);
        assert!(group.members[&10102].actor_state.iter().any(|s| s == "defaulted"));
        assert!(lagpoller::lacp_bundled(&group.members[&10101].actor_state));

        // core1 carries a healthy 2×10G bundle to each dist switch; both ends
        // are monitored, so the far-end cross-check must come back clean.
        let core1 = decode("core1.mock.jaspy");
        assert_eq!(core1.groups.len(), 2);
        let dist1_lags = decode("dist1.mock.jaspy");
        assert_eq!(dist1_lags.groups.len(), 1);

        let topo_ref = &topo;
        let meta_for = |name: &str, group: &lagpoller::LagGroup| -> std::collections::HashMap<i64, lagpoller::MemberMeta> {
            let dev = topo_ref.devices.iter().find(|d| d.name == name).unwrap();
            group.members.keys().map(|ifindex| {
                let iface = dev.interfaces.iter().find(|i| i.ifindex == *ifindex).unwrap();
                (*ifindex, lagpoller::MemberMeta {
                    name: iface.name.to_string(),
                    connected_to_fqdn: iface.peer.map(|(peer, _)| format!("{}.{}", peer, DOMAIN)),
                })
            }).collect()
        };
        let mut peer_lags = std::collections::HashMap::new();
        peer_lags.insert("dist1.mock.jaspy".to_string(), dist1_lags.clone());
        peer_lags.insert("core1.mock.jaspy".to_string(), core1.clone());

        let group = &core1.groups[&5001];
        assert!(group.members.values().all(|m| lagpoller::lacp_bundled(&m.actor_state)));
        assert_eq!(
            lagpoller::port_channel_warnings(group, &meta_for("core1", group), &peer_lags),
            Vec::<String>::new(),
            "core1 Po1 to dist1 must warn nothing"
        );
        let group = &dist1_lags.groups[&5001];
        assert_eq!(
            lagpoller::port_channel_warnings(group, &meta_for("dist1", group), &peer_lags),
            Vec::<String>::new(),
            "dist1 Po1 to core1 must warn nothing"
        );

        // dist2 Po1 to core1 has one member (Te1/1/5) with its cable pulled:
        // still a configured member (2 total) but detached / not bundled.
        let dist2_lags = decode("dist2.mock.jaspy");
        let group = &dist2_lags.groups[&5001];
        assert_eq!(group.members.len(), 2, "the down member is still a configured member");
        assert!(lagpoller::lacp_bundled(&group.members[&10101].actor_state), "the surviving member bundles");
        assert!(!lagpoller::lacp_bundled(&group.members[&10105].actor_state), "the down member cannot bundle");
        peer_lags.insert("dist2.mock.jaspy".to_string(), dist2_lags.clone());
        assert_eq!(
            lagpoller::port_channel_warnings(group, &meta_for("dist2", group), &peer_lags),
            vec!["member-not-bundled:Te1/1/5".to_string()],
            "the down uplink shows as an unbundled member (issues.rs upgrades this to a link-down verdict)"
        );

        // wlc1 Po1 <-> dist2 Po2: a symmetric single-member SFP+ bundle. Each end
        // has exactly one healthy member wired to the other, so the far-end
        // cross-check passes (matching member count) and the only warning is
        // single-member — the demo data for the enriched single-member Issue.
        let wlc1_lags = decode("wlc1.mock.jaspy");
        peer_lags.insert("wlc1.mock.jaspy".to_string(), wlc1_lags.clone());
        let group = &wlc1_lags.groups[&5001];
        assert_eq!(group.members.len(), 1, "wlc1 Po1 is a single-member SFP+ uplink");
        assert_eq!(
            lagpoller::port_channel_warnings(group, &meta_for("wlc1", group), &peer_lags),
            vec!["single-member".to_string()],
            "a healthy symmetric single-member bundle warns only single-member"
        );
        let group = &dist2_lags.groups[&5002];
        assert_eq!(group.members.len(), 1, "dist2 Po2 mirrors wlc1's single member");
        assert_eq!(
            lagpoller::port_channel_warnings(group, &meta_for("dist2", group), &peer_lags),
            vec!["single-member".to_string()],
            "the mirrored far end also warns only single-member"
        );
    }

    #[test]
    fn every_device_answers_the_critical_tables() {
        let topo = build();
        for dev in topo.devices.iter() {
            for table in ["IF-MIB::ifTable", "IF-MIB::ifXTable"] {
                let resp = topo.table(&dev.fqdn(), None, table, 0.0).unwrap();
                assert!(!resp.entries.is_empty(), "{} {} must be non-empty (device-up signal)", dev.name, table);
            }
        }
    }

    #[test]
    fn counters_are_monotonic() {
        for (elapsed_early, elapsed_late) in [(0.0, 1.0), (59.9, 60.1), (100.0, 1000.0)] {
            for kind in [10, 11, 15, 1] {
                let early = counter(3, 10101, kind, elapsed_early);
                let late = counter(3, 10101, kind, elapsed_late);
                assert!(early <= late, "counter regressed for kind {}", kind);
            }
        }
        // Uplink octets must actually grow over a poll interval.
        assert!(counter(0, 10101, 10, 60.0) > counter(0, 10101, 10, 0.0));
        // Error counters are monotonic too.
        assert!(error_counter(3, 10201, 1, 600.0) >= error_counter(3, 10201, 1, 60.0));
    }

    #[test]
    fn flap_function_has_expected_period() {
        let topo = build();
        let dist1 = topo.devices.iter().find(|d| d.name == "dist1").unwrap();
        let flapping = dist1.interfaces.iter().find(|i| i.flaps).unwrap();
        assert!(iface_up(flapping, 0.0));
        assert!(iface_up(flapping, 59.0));
        assert!(!iface_up(flapping, 60.0));
        assert!(!iface_up(flapping, 119.0));
        assert!(iface_up(flapping, 120.0));

        // Both ends of the flapping link agree at all times.
        let a02 = topo.devices.iter().find(|d| d.name == "access-hall-a-02").unwrap();
        let other_end = a02.interfaces.iter().find(|i| i.flaps).unwrap();
        for t in [0.0, 30.0, 61.0, 90.0, 121.0] {
            assert_eq!(iface_up(flapping, t), iface_up(other_end, t), "flap ends disagree at t={}", t);
        }
    }

    #[test]
    fn non_flapping_interfaces_hold_static_state() {
        let topo = build();
        let a01 = topo.devices.iter().find(|d| d.name == "access-hall-a-01").unwrap();
        for iface in a01.interfaces.iter().filter(|i| !i.flaps) {
            assert_eq!(iface_up(iface, 0.0), iface_up(iface, 3600.0));
        }
        // The static mix contains both up and down access ports.
        assert!(a01.interfaces.iter().any(|i| i.peer.is_none() && i.up));
        assert!(a01.interfaces.iter().any(|i| i.peer.is_none() && !i.up));
    }

    #[test]
    fn lldp_adjacency_is_reciprocal_and_resolvable() {
        let topo = build();
        for dev in topo.devices.iter() {
            for iface in dev.interfaces.iter() {
                if let Some((peer_name, peer_iface)) = iface.peer {
                    let peer = topo.devices.iter().find(|d| d.name == peer_name)
                        .unwrap_or_else(|| panic!("{}: peer {} not in topology", dev.name, peer_name));
                    assert!(
                        peer.interfaces.iter().any(|i| i.name == peer_iface),
                        "{}: peer interface {}:{} missing", dev.name, peer_name, peer_iface
                    );
                }
            }
            // Every neighbor visible in the LLDP rem table maps back: the
            // remote device sees this device on the named interface.
            for (local_ifindex, peer_idx, peer_iface) in topo.neighbors(dev) {
                let peer = &topo.devices[peer_idx];
                let reciprocal = topo.neighbors(peer);
                let local_iface = dev.interfaces.iter().find(|i| i.ifindex == local_ifindex).unwrap();
                assert!(
                    reciprocal.iter().any(|(rem_ifindex, rem_peer_idx, rem_peer_iface)| {
                        topo.devices[*rem_peer_idx].name == dev.name
                            && *rem_peer_iface == local_iface.name
                            && peer.interfaces.iter().any(|i| i.ifindex == *rem_ifindex && i.name == peer_iface)
                    }),
                    "asymmetric link {}:{} -> {}:{}", dev.name, local_iface.name, peer.name, peer_iface
                );
            }
        }
    }

    #[test]
    fn firewall_advertises_neighbors_without_local_lldp_data() {
        // fw1 models FortiOS: lldpRemTable is populated but lldpLocPortTable
        // walks to nothing and there is no lldpLocChassisId — the exact shape
        // that exercises discovery's lldpRemLocalPortNum==ifIndex fallback.
        let topo = build();
        let fw = topo.devices.iter().find(|d| d.name == "fw1").unwrap();
        assert!(!fw.exposes_lldp_local);
        let loc = topo.table(&fw.fqdn(), None, "LLDP-MIB::lldpLocPortTable", 0.0).unwrap();
        assert!(loc.entries.is_empty(), "firewall must expose an empty lldpLocPortTable");
        let rem = topo.table(&fw.fqdn(), None, "LLDP-MIB::lldpRemTable", 0.0).unwrap();
        assert!(!rem.entries.is_empty(), "firewall must still advertise LLDP neighbors");
        assert!(topo.object(&fw.fqdn(), None, "LLDP-MIB::lldpLocChassisId", 0.0).is_none());
    }

    #[test]
    fn base_macs_are_unique() {
        let topo = build();
        let macs: std::collections::HashSet<String> =
            (0..topo.devices.len()).map(base_mac).collect();
        assert_eq!(macs.len(), topo.devices.len());
    }

    #[test]
    fn sensor_values_stay_in_bounds() {
        for dev_idx in 0..8 {
            for sensor_idx in 0..3 {
                for t in [0.0, 10.0, 100.0, 1000.0, 86_400.0] {
                    let milli = sensor_value_milli(dev_idx, sensor_idx, t);
                    assert!((38_000..=46_000).contains(&milli), "sensor out of range: {}", milli);
                }
            }
        }
    }

    #[test]
    fn sensor_style_gates_the_two_sensor_tables() {
        let topo = build();
        let cisco = topo.devices.iter().find(|d| d.sensor_style == SensorStyle::Cisco).unwrap();
        let standard = topo.devices.iter().find(|d| d.sensor_style == SensorStyle::Standard).unwrap();
        let none = topo.devices.iter().find(|d| d.sensor_style == SensorStyle::None).unwrap();
        let phy = "ENTITY-SENSOR-MIB::entPhySensorTable";
        let cisco_t = "CISCO-ENTITY-SENSOR-MIB::entSensorValueTable";
        assert!(topo.table(&cisco.fqdn(), None, cisco_t, 0.0).is_some());
        assert!(topo.table(&cisco.fqdn(), None, phy, 0.0).is_none());
        assert!(topo.table(&standard.fqdn(), None, phy, 0.0).is_some());
        assert!(topo.table(&standard.fqdn(), None, cisco_t, 0.0).is_none());
        assert!(topo.table(&none.fqdn(), None, phy, 0.0).is_none());
        assert!(topo.table(&none.fqdn(), None, cisco_t, 0.0).is_none());
    }

    #[test]
    fn vlan_style_gates_the_vlan_tables() {
        let topo = build();
        let cisco = topo.devices.iter().find(|d| d.vlan_style == VlanStyle::Cisco).unwrap();
        let qbridge = topo.devices.iter().find(|d| d.vlan_style == VlanStyle::QBridge).unwrap();
        let none = topo.devices.iter().find(|d| d.vlan_style == VlanStyle::None).unwrap();
        let cisco_tables = ["CISCO-VTP-MIB::jaspyVlanTrunkPortTable", "CISCO-VTP-MIB::vtpVlanTable", "CISCO-VLAN-MEMBERSHIP-MIB::vmMembershipTable"];
        let qbridge_tables = ["Q-BRIDGE-MIB::dot1qPortVlanTable", "Q-BRIDGE-MIB::dot1qVlanCurrentTable", "Q-BRIDGE-MIB::dot1qVlanStaticTable"];
        for table in cisco_tables {
            assert!(topo.table(&cisco.fqdn(), None, table, 0.0).is_some(), "{} must answer on cisco", table);
            assert!(topo.table(&qbridge.fqdn(), None, table, 0.0).is_none(), "{} must 404 on qbridge", table);
            assert!(topo.table(&none.fqdn(), None, table, 0.0).is_none(), "{} must 404 on none", table);
        }
        for table in qbridge_tables {
            assert!(topo.table(&qbridge.fqdn(), None, table, 0.0).is_some(), "{} must answer on qbridge", table);
            assert!(topo.table(&cisco.fqdn(), None, table, 0.0).is_none(), "{} must 404 on cisco", table);
            assert!(topo.table(&none.fqdn(), None, table, 0.0).is_none(), "{} must 404 on none", table);
        }
        // The base-community dot1dBasePortTable form (no vlan) is the
        // Q-BRIDGE translation table and only answers for QBridge devices;
        // the per-vlan STP form keeps working for everyone with stp_vlans.
        assert!(topo.table(&qbridge.fqdn(), None, "BRIDGE-MIB::dot1dBasePortTable", 0.0).is_some());
        assert!(topo.table(&cisco.fqdn(), None, "BRIDGE-MIB::dot1dBasePortTable", 0.0).is_none());
        let stp_dev = topo.devices.iter().find(|d| !d.stp_vlans.is_empty()).unwrap();
        assert!(topo.table(&stp_dev.fqdn(), Some(stp_dev.stp_vlans[0]), "BRIDGE-MIB::dot1dBasePortTable", 0.0).is_some());
    }

    // The generated tables must decode through the real vlanpoller logic:
    // trunks native 1 + tagged = device vlans minus 1, access ports on their
    // access_vlan. Guards the mock and the decoder against drifting apart.
    #[test]
    fn vlan_names_resolve_via_vlanpoller() {
        use crate::collectors::vlanpoller::{cisco_vlan_names, qbridge_vlan_names};
        let topo = build();
        let cisco = topo.devices.iter().find(|d| d.vlan_style == VlanStyle::Cisco).unwrap();
        let vtp = topo.table(&cisco.fqdn(), None, "CISCO-VTP-MIB::vtpVlanTable", 0.0).unwrap();
        let names = cisco_vlan_names(&vtp);
        for vlan in cisco.vlans.iter() {
            assert_eq!(names.get(vlan).map(String::as_str), Some(format!("mock-vlan-{}", vlan).as_str()));
        }
        let qbridge = topo.devices.iter().find(|d| d.vlan_style == VlanStyle::QBridge).unwrap();
        let vlan_static = topo.table(&qbridge.fqdn(), None, "Q-BRIDGE-MIB::dot1qVlanStaticTable", 0.0).unwrap();
        let names = qbridge_vlan_names(&vlan_static);
        for vlan in qbridge.vlans.iter().filter(|v| **v != 20) {
            assert_eq!(names.get(vlan).map(String::as_str), Some(format!("mock-vlan-{}", vlan).as_str()));
        }
        // The deliberate name mismatch that feeds the VLANs page warning.
        assert_eq!(names.get(&20).map(String::as_str), Some("guest-legacy"));
    }

    #[test]
    fn vlan_tables_decode_via_vlanpoller() {
        use crate::collectors::vlanpoller::{decode_cisco, decode_qbridge};
        let topo = build();

        let cisco = topo.devices.iter().find(|d| d.name == "access-hall-a-01").unwrap();
        let trunk = topo.table(&cisco.fqdn(), None, "CISCO-VTP-MIB::jaspyVlanTrunkPortTable", 0.0).unwrap();
        let vtp = topo.table(&cisco.fqdn(), None, "CISCO-VTP-MIB::vtpVlanTable", 0.0).unwrap();
        let membership = topo.table(&cisco.fqdn(), None, "CISCO-VLAN-MEMBERSHIP-MIB::vmMembershipTable", 0.0).unwrap();
        let decoded = decode_cisco(&trunk, Some(&vtp), Some(&membership));
        for iface in cisco.interfaces.iter() {
            let port = &decoded[&iface.ifindex];
            if iface.peer.is_some() {
                assert_eq!(port.native_vlan, Some(TRUNK_NATIVE_VLAN), "{} trunk native", iface.name);
                assert_eq!(port.tagged_vlans, vec![10, 20], "{} trunk tagged", iface.name);
            } else {
                assert_eq!(port.native_vlan, Some(access_vlan(iface)), "{} access vlan", iface.name);
                assert!(port.tagged_vlans.is_empty(), "{} access tagged", iface.name);
            }
        }

        let qbridge = topo.devices.iter().find(|d| d.vlan_style == VlanStyle::QBridge).unwrap();
        let pvid = topo.table(&qbridge.fqdn(), None, "Q-BRIDGE-MIB::dot1qPortVlanTable", 0.0).unwrap();
        let current = topo.table(&qbridge.fqdn(), None, "Q-BRIDGE-MIB::dot1qVlanCurrentTable", 0.0).unwrap();
        let base = topo.table(&qbridge.fqdn(), None, "BRIDGE-MIB::dot1dBasePortTable", 0.0).unwrap();
        let decoded = decode_qbridge(Some(&pvid), Some(&current), &base);
        for iface in qbridge.interfaces.iter() {
            let port = &decoded[&iface.ifindex];
            if iface.peer.is_some() {
                assert_eq!(port.native_vlan, Some(TRUNK_NATIVE_VLAN), "{} trunk native", iface.name);
                assert_eq!(port.tagged_vlans, vec![10, 20], "{} trunk tagged", iface.name);
            } else {
                assert_eq!(port.native_vlan, Some(access_vlan(iface)), "{} access vlan", iface.name);
                assert!(port.tagged_vlans.is_empty(), "{} access tagged", iface.name);
            }
        }
    }

    #[test]
    fn entphysical_table_classifies_media() {
        use crate::collectors::entity_media::classify_media;
        let topo = build();

        // An access switch: Gi ports are copper, its 10G uplink is an SFP+ cage.
        let access = topo.devices.iter().find(|d| d.name == "access-hall-a-01").unwrap();
        let phys = topo.table(&access.fqdn(), None, "ENTITY-MIB::entPhysicalTable", 0.0).unwrap();
        let media = classify_media(&phys.entries);
        assert_eq!(media.get("Gi1/0/1").map(String::as_str), Some("copper"));
        assert_eq!(media.get("Gi1/0/8").map(String::as_str), Some("copper"));
        // Uplink Te1/1/1 (ifindex 10101, divisible by 3) is an empty cage.
        assert_eq!(media.get("Te1/1/1").map(String::as_str), Some("sfp"));

        // core1's 10G uplinks are all SFP cages (some populated); no copper.
        let core = topo.devices.iter().find(|d| d.name == "core1").unwrap();
        let phys = topo.table(&core.fqdn(), None, "ENTITY-MIB::entPhysicalTable", 0.0).unwrap();
        let media = classify_media(&phys.entries);
        assert!(media.values().all(|m| m.starts_with("sfp")), "core1 media: {:?}", media);
        // Populated cages report the optic descr, including both a 10G (SFP+) and
        // a 1G (SFP) optic so the SFP-vs-SFP+ distinction is exercised.
        assert!(media.values().any(|m| m.contains("10GBase")), "expected a 10G optic: {:?}", media);
        assert!(media.values().any(|m| m.contains("1000Base")), "expected a 1G optic: {:?}", media);
        assert!(media.values().any(|m| m == "sfp"), "expected an empty cage: {:?}", media);
        // Port-channels are logical: no physical entity, no media.
        assert!(!media.contains_key("Po1"));
    }

    #[test]
    fn hex_bitmap_roundtrips_through_vlanpoller_decoder() {
        use crate::collectors::vlanpoller::{bitmap_values, parse_hex_octets};
        for (values, base, len) in [
            (vec![1i64, 10, 20], 0i64, 128usize),
            (vec![1, 5, 6], 1, 4),
            (vec![1024, 1100], 1024, 128),
            (Vec::new(), 0, 8),
        ] {
            let encoded = hex_bitmap(&values, base, len);
            assert_eq!(bitmap_values(&parse_hex_octets(&encoded), base), values, "roundtrip {:?}", values);
        }
    }

    #[test]
    fn stp_role_matrix() {
        let topo = build();
        let core1 = topo.devices.iter().find(|d| d.name == "core1").unwrap();
        for iface in core1.interfaces.iter() {
            assert_eq!(stp_role(core1, iface), "designated", "root bridge has no root ports");
        }
        // Every primary uplink is a root port, including a-02's flapping one.
        let a02 = topo.devices.iter().find(|d| d.name == "access-hall-a-02").unwrap();
        let a02_uplink = a02.interfaces.iter().find(|i| i.alias == "uplink").unwrap();
        assert!(a02_uplink.flaps);
        assert_eq!(stp_role(a02, a02_uplink), "root");
        // a-02's redundant backup uplink to core1 is the blocked alternate;
        // core1's end of that same link is designated (physically consistent).
        let a02_backup = a02.interfaces.iter().find(|i| i.alias.starts_with("backup uplink")).unwrap();
        assert_eq!(stp_role(a02, a02_backup), "alternate");
        let core1 = topo.devices.iter().find(|d| d.name == "core1").unwrap();
        let core1_backup_end = core1.interfaces.iter().find(|i| i.alias.starts_with("backup downlink")).unwrap();
        assert_eq!(stp_role(core1, core1_backup_end), "designated");
        // Downlinks stay designated — even dist1's flapping one, which is the
        // far end of a-02's *root* port and must not be blocked.
        let dist1 = topo.devices.iter().find(|d| d.name == "dist1").unwrap();
        let flapping_downlink = dist1.interfaces.iter().find(|i| i.flaps).unwrap();
        assert_eq!(stp_role(dist1, flapping_downlink), "designated");
        let downlink = dist1.interfaces.iter().find(|i| i.alias.contains("hall a 01")).unwrap();
        assert_eq!(stp_role(dist1, downlink), "designated");
    }

    #[test]
    fn stp_ports_fold_bundled_members_into_the_port_channel() {
        let topo = build();
        // dist1: members 10101/10105 are LACP-bundled — only Po1 represents
        // them in the STP tables, and it takes the root-port role.
        let dist1 = topo.devices.iter().find(|d| d.name == "dist1").unwrap();
        let ports = Topology::stp_ports(dist1);
        assert!(ports.iter().all(|(_, i)| i.ifindex != 10101 && i.ifindex != 10105));
        let (_, po) = ports.iter().find(|(_, i)| i.ifindex == 5001).unwrap();
        assert_eq!(stp_role(dist1, po), "root");
        // a-02: the bundled member folds into Po1 (root), but the defaulted
        // backup member never formed a bundle and stays the individual
        // blocked port — the trees keep their alternate link.
        let a02 = topo.devices.iter().find(|d| d.name == "access-hall-a-02").unwrap();
        let ports = Topology::stp_ports(a02);
        assert!(ports.iter().all(|(_, i)| i.ifindex != 10101), "bundled member folds into Po1");
        let (_, backup) = ports.iter().find(|(_, i)| i.ifindex == 10102).unwrap();
        assert_eq!(stp_role(a02, backup), "alternate");
        let (_, po) = ports.iter().find(|(_, i)| i.ifindex == 5001).unwrap();
        assert_eq!(stp_role(a02, po), "root");
    }

    #[test]
    fn stp_scalar_objects_gated_on_vlan_membership() {
        let topo = build();
        let dist1 = topo.devices.iter().find(|d| d.name == "dist1").unwrap();
        let scalars = [
            "BRIDGE-MIB::dot1dStpDesignatedRoot",
            "BRIDGE-MIB::dot1dStpRootCost",
            "BRIDGE-MIB::dot1dStpRootPort",
            "BRIDGE-MIB::dot1dStpTopChanges",
            "BRIDGE-MIB::dot1dStpTimeSinceTopologyChange",
        ];
        for object in scalars {
            assert!(topo.object(&dist1.fqdn(), Some(10), object, 30.0).is_some(), "{} on member vlan", object);
            assert!(topo.object(&dist1.fqdn(), Some(999), object, 30.0).is_none(), "{} on non-member vlan", object);
            assert!(topo.object(&dist1.fqdn(), None, object, 30.0).is_none(), "{} without vlan", object);
        }
        // Non-STP devices answer nothing per-vlan.
        let wlc = topo.devices.iter().find(|d| d.name == "wlc1").unwrap();
        assert!(topo.object(&wlc.fqdn(), Some(10), "BRIDGE-MIB::dot1dStpRootCost", 0.0).is_none());
    }

    #[test]
    fn stp_designated_root_is_core1_bridge_id() {
        let topo = build();
        // Every STP device on vlan 10 reports core1 (devices[0]) as the root,
        // with priority 24576 + vlan.
        for name in ["core1", "dist1", "access-hall-a-01", "access-hall-a-02"] {
            let dev = topo.devices.iter().find(|d| d.name == name).unwrap();
            let root = topo.object(&dev.fqdn(), Some(10), "BRIDGE-MIB::dot1dStpDesignatedRoot", 0.0).unwrap();
            assert_eq!(root.as_str().unwrap(), format!("60 0a {}", base_mac_spaced(0)), "{}", name);
        }
        // Root port: core1 has none (0); dist1's is its uplink bridge port.
        assert_eq!(topo.object("core1.mock.jaspy", Some(10), "BRIDGE-MIB::dot1dStpRootPort", 0.0).unwrap(), 0);
        let dist1 = topo.devices.iter().find(|d| d.name == "dist1").unwrap();
        let uplink_port = Topology::stp_ports(dist1).into_iter()
            .find(|(_, i)| i.alias.starts_with("uplink")).map(|(p, _)| p).unwrap();
        assert_eq!(topo.object(&dist1.fqdn(), Some(10), "BRIDGE-MIB::dot1dStpRootPort", 0.0).unwrap(), uplink_port);
    }

    #[test]
    fn stp_vlan63_access_reports_superior_off_fleet_root() {
        let topo = build();
        // a-01 hears a superior (priority 4159) off-fleet root on VLAN 63,
        // while core1 still elects itself — the root-mismatch demo. dist1 does
        // not run 63, so a-01's uplink upstream is unresolved (orphan).
        let a01 = "access-hall-a-01.mock.jaspy";
        let root = topo.object(a01, Some(63), "BRIDGE-MIB::dot1dStpDesignatedRoot", 0.0).unwrap();
        assert_eq!(root.as_str().unwrap(), "10 3f 5c e1 76 60 9b 00"); // priority 4159, off-fleet MAC
        let core_root = topo.object("core1.mock.jaspy", Some(63), "BRIDGE-MIB::dot1dStpDesignatedRoot", 0.0).unwrap();
        assert_eq!(core_root.as_str().unwrap(), format!("60 3f {}", base_mac_spaced(0))); // core1 elects itself
        // dist1 does not participate in VLAN 63.
        assert!(topo.object("dist1.mock.jaspy", Some(63), "BRIDGE-MIB::dot1dStpDesignatedRoot", 0.0).is_none());
    }

    #[test]
    fn stp_vlan30_is_a_directly_linked_split_brain() {
        let topo = build();
        // On VLAN 30 both core1 and dist1 report themselves as root (cost 0,
        // no root port) — a split brain. They are directly linked (Po1), so it
        // is the genuine, critical case.
        let core_root = topo.object("core1.mock.jaspy", Some(30), "BRIDGE-MIB::dot1dStpDesignatedRoot", 0.0).unwrap();
        assert_eq!(core_root.as_str().unwrap(), format!("60 1e {}", base_mac_spaced(0))); // 24606, core1 mac
        let dist_root = topo.object("dist1.mock.jaspy", Some(30), "BRIDGE-MIB::dot1dStpDesignatedRoot", 0.0).unwrap();
        assert_eq!(dist_root.as_str().unwrap(), format!("60 1e {}", base_mac_spaced(1))); // 24606, dist1 mac
        assert_eq!(topo.object("dist1.mock.jaspy", Some(30), "BRIDGE-MIB::dot1dStpRootCost", 0.0).unwrap(), 0);
        assert_eq!(topo.object("dist1.mock.jaspy", Some(30), "BRIDGE-MIB::dot1dStpRootPort", 0.0).unwrap(), 0);
        // dist1's VLAN 30 role table has no root port (all designated) → it
        // computes as a root, not a child.
        let dist1 = topo.devices.iter().find(|d| d.name == "dist1").unwrap();
        let roles = topo.table(&dist1.fqdn(), None, "CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleTable", 0.0).unwrap();
        let vlan30_roles: Vec<_> = roles.entries.iter()
            .filter(|e| e.index.get("CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleInstanceIndex").copied() == Some(30))
            .filter_map(|e| match e.objects.get("CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleValue") {
                Some(SNMPBotResultEntryObjectValue::Str(s)) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert!(!vlan30_roles.is_empty());
        assert!(vlan30_roles.iter().all(|r| *r != "root"), "dist1 vlan30 should self-root: {:?}", vlan30_roles);
    }

    #[test]
    fn cdp_table_reports_off_fleet_phones_on_access_ports() {
        let topo = build();
        // An access switch reports IP phones (SEP…) on its live desk ports —
        // off-fleet neighbors that never resolve to a monitored device.
        let a01 = topo.devices.iter().find(|d| d.name == "access-hall-a-01").unwrap();
        let cdp = topo.table(&a01.fqdn(), None, "CISCO-CDP-MIB::cdpCacheTable", 0.0).unwrap();
        assert!(!cdp.entries.is_empty(), "access switch should report CDP phones");
        for e in cdp.entries.iter() {
            match e.objects.get("CISCO-CDP-MIB::cdpCacheDeviceId") {
                Some(SNMPBotResultEntryObjectValue::Str(s)) => assert!(s.starts_with("SEP"), "device id {}", s),
                _ => panic!("expected a CDP device id string"),
            }
        }
        // Phones sit only on the odd-ifIndex desk ports (even ports model plain
        // PCs with no CDP/LLDP), and none are uplinks/LAG members.
        assert!(cdp.entries.iter().all(|e| {
            let ifindex = e.index.get("CISCO-CDP-MIB::cdpCacheIfIndex").copied().unwrap_or(0);
            ifindex % 2 == 1 && a01.interfaces.iter().any(|i| i.ifindex == ifindex && i.peer.is_none())
        }));
        // The core (no access ports) reports no CDP.
        let core = topo.devices.iter().find(|d| d.name == "core1").unwrap();
        assert!(topo.table(&core.fqdn(), None, "CISCO-CDP-MIB::cdpCacheTable", 0.0).unwrap().entries.is_empty());
    }

    #[test]
    fn stp_style_gates_the_stp_tables() {
        let topo = build();
        let cisco = topo.devices.iter().find(|d| d.stp_style == StpStyle::Cisco).unwrap();
        let rpvst = topo.devices.iter().find(|d| d.stp_style == StpStyle::Rpvst).unwrap();
        let rpvst_vlan = rpvst.stp_vlans[0];

        // The Cisco tables 404 on the RPVST device and vice versa.
        assert!(topo.table(&rpvst.fqdn(), None, "CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleTable", 0.0).is_none());
        assert!(topo.table(&rpvst.fqdn(), Some(rpvst_vlan), "BRIDGE-MIB::dot1dStpPortTable", 0.0).is_none());
        assert!(topo.object(&rpvst.fqdn(), Some(rpvst_vlan), "BRIDGE-MIB::dot1dStpRootCost", 0.0).is_none());
        assert!(topo.table(&cisco.fqdn(), None, "HP-ICF-RPVST-MIB::jaspyRpvstPortVlanRoleTable", 0.0).is_none());
        assert!(topo.table(&cisco.fqdn(), None, "HP-ICF-RPVST-MIB::hpicfRpvstVlanTable", 0.0).is_none());

        // The RPVST device serves the HP views on the base community: its
        // uplink is the root port toward core1, port index == ifIndex.
        let roles = topo.table(&rpvst.fqdn(), None, "HP-ICF-RPVST-MIB::jaspyRpvstPortVlanRoleTable", 0.0).unwrap();
        let uplink = rpvst.interfaces.iter().find(|i| i.alias == "uplink").unwrap();
        let root_rows: Vec<_> = roles.entries.iter().filter(|e| {
            matches!(e.objects.get("HP-ICF-RPVST-MIB::hpicfRpvstPortVlanRole"), Some(SNMPBotResultEntryObjectValue::Str(s)) if s == "root")
        }).collect();
        assert_eq!(root_rows.len(), 1);
        assert_eq!(root_rows[0].index["HP-ICF-RPVST-MIB::hpicfRpvstPortIndex"], uplink.ifindex);

        let vlan_table = topo.table(&rpvst.fqdn(), None, "HP-ICF-RPVST-MIB::hpicfRpvstVlanTable", 30.0).unwrap();
        assert_eq!(vlan_table.entries.len(), rpvst.stp_vlans.len());
        match vlan_table.entries[0].objects.get("HP-ICF-RPVST-MIB::hpicfRpvstVlanRootMacAddress") {
            Some(SNMPBotResultEntryObjectValue::Str(mac)) => assert_eq!(*mac, base_mac_spaced(0), "core1 is the root"),
            other => panic!("root mac must be a string, got {:?}", other.is_some()),
        }
    }

    #[test]
    fn stp_bridge_ports_all_resolve_via_base_port_table() {
        let topo = build();
        for dev in topo.devices.iter().filter(|d| d.stp_style == StpStyle::Cisco && !d.stp_vlans.is_empty()) {
            for vlan in dev.stp_vlans.iter() {
                let roles = topo.table(&dev.fqdn(), None, "CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleTable", 0.0).unwrap();
                let base = topo.table(&dev.fqdn(), Some(*vlan), "BRIDGE-MIB::dot1dBasePortTable", 0.0).unwrap();
                let stp = topo.table(&dev.fqdn(), Some(*vlan), "BRIDGE-MIB::dot1dStpPortTable", 0.0).unwrap();
                let base_ports: std::collections::HashSet<i64> =
                    base.entries.iter().map(|e| e.index["BRIDGE-MIB::dot1dBasePort"]).collect();
                for entry in roles.entries.iter().filter(|e| e.index["CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleInstanceIndex"] == *vlan) {
                    let port = entry.index["CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRolePortIndex"];
                    assert!(base_ports.contains(&port), "{} vlan {} port {} missing from base table", dev.name, vlan, port);
                }
                assert_eq!(base.entries.len(), stp.entries.len());
            }
        }
    }

    #[test]
    fn objects_answer_for_every_device() {
        let topo = build();
        for (idx, dev) in topo.devices.iter().enumerate() {
            assert!(topo.object(&dev.fqdn(), None, "SNMPv2-MIB::sysDescr", 0.0).is_some());
            assert!(topo.object(&dev.fqdn(), None, "NO-SUCH-MIB::thing", 0.0).is_none());
            if dev.exposes_lldp_local {
                let mac = topo.object(&dev.fqdn(), None, "BRIDGE-MIB::dot1dBaseBridgeAddress", 0.0).unwrap();
                assert_eq!(mac.as_str().unwrap(), base_mac_spaced(idx));
                assert!(topo.object(&dev.fqdn(), None, "LLDP-MIB::lldpLocChassisId", 0.0).is_some());
            } else {
                // FortiOS-style firewall: neither base_mac source is present.
                assert!(topo.object(&dev.fqdn(), None, "BRIDGE-MIB::dot1dBaseBridgeAddress", 0.0).is_none());
                assert!(topo.object(&dev.fqdn(), None, "LLDP-MIB::lldpLocChassisId", 0.0).is_none());
            }
        }
        assert!(topo.object("ghost.mock.jaspy", None, "SNMPv2-MIB::sysDescr", 0.0).is_none());
    }

    #[test]
    fn ifoperstatus_values_are_strings() {
        let topo = build();
        let resp = topo.table(ROOT_DEVICE, None, "IF-MIB::ifTable", 0.0).unwrap();
        for entry in resp.entries.iter() {
            match entry.objects.get("IF-MIB::ifOperStatus") {
                Some(SNMPBotResultEntryObjectValue::Str(s)) => assert!(s == "up" || s == "down"),
                other => panic!("ifOperStatus must be a string, got {:?}", other.is_some()),
            }
        }
    }
}
