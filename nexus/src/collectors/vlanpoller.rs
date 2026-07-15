// In-process per-interface VLAN membership collector: native (untagged) VLAN
// and tagged VLAN list per port.
//
// Sources, in order of preference:
//   1. Cisco: CISCO-VTP-MIB::vlanTrunkPortTable (trunk native + allowed-VLAN
//      bitmaps, intersected with the VLANs that actually exist per
//      CISCO-VTP-MIB::vtpVlanTable — default trunks report all 4096 bits set)
//      plus CISCO-VLAN-MEMBERSHIP-MIB::vmMembershipTable (access-port VLAN).
//   2. Q-BRIDGE-MIB fallback for non-Cisco gear: dot1qPvid per bridge port and
//      dot1qVlanCurrentTable (or dot1qVlanStaticTable) egress/untagged PortList
//      bitmaps, translated to ifIndex via BRIDGE-MIB::dot1dBasePortTable.
//
// Results live only in the in-memory `VlanStore` (no DB, no Prometheus): the
// data is re-polled on an interval and can also be refreshed on demand per
// device via POST /api/v1/devices/<fqdn>/vlans/poll, which queues the fqdn in
// `VlanPollerControl` for the supervisor loop to pick up on its next 1s tick.
extern crate serde_json;

use crate::collectors::entitypoller::{fetch_table, interruptible_sleep, obj_i64, obj_str};
use crate::collectors::poller::SNMPBotResponse;
use crate::db;
use crate::utilities::tools;
use rand::prelude::*;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{atomic, Arc, Mutex};
use std::thread;
use std::time;

// ---------------------------------------------------------------------------
// Shared store: latest VLAN membership per device, replaced on each successful
// poll (a failed poll keeps the previous data rather than blanking the UI).
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct InterfaceVlans {
    pub native_vlan: Option<i64>,
    // Sorted, deduplicated, never contains the native VLAN.
    pub tagged_vlans: Vec<i64>,
}

pub struct VlanStore {
    devices: HashMap<String, HashMap<i64, InterfaceVlans>>, // fqdn -> ifIndex -> vlans
}

impl VlanStore {
    pub fn new() -> VlanStore {
        VlanStore { devices: HashMap::new() }
    }

    fn replace_device(&mut self, fqdn: String, interfaces: HashMap<i64, InterfaceVlans>) {
        self.devices.insert(fqdn, interfaces);
    }

    fn retain(&mut self, keep: &HashSet<String>) {
        self.devices.retain(|fqdn, _| keep.contains(fqdn));
    }

    // Snapshot for the API route; unknown fqdn and not-yet-polled both empty.
    pub fn device_vlans(&self, fqdn: &str) -> HashMap<i64, InterfaceVlans> {
        self.devices.get(fqdn).cloned().unwrap_or_default()
    }
}

// Poll-now queue: fqdns stay in `pending` from the POST until their triggered
// poll finishes, so a repeated POST while one is queued/running answers 409.
pub struct VlanPollerControl {
    pub pending: HashSet<String>,
}

impl VlanPollerControl {
    pub fn new() -> VlanPollerControl {
        VlanPollerControl { pending: HashSet::new() }
    }
}

// ---------------------------------------------------------------------------
// Bitmap decoding
// ---------------------------------------------------------------------------

// snmpbot renders OCTET STRING values as lowercase space-separated hex bytes
// ("7f ff 00 …"). Any unparseable token voids the whole value.
pub(crate) fn parse_hex_octets(value: &str) -> Vec<u8> {
    let mut octets = Vec::new();
    for token in value.split_whitespace() {
        match u8::from_str_radix(token, 16) {
            Ok(octet) => octets.push(octet),
            Err(_) => return Vec::new(),
        }
    }
    octets
}

// Both Cisco VLAN bitmaps and Q-BRIDGE PortLists are MSB-first: bit j of octet
// i (counting from the most significant bit) represents value base + i*8 + j.
// Cisco vlansEnabled uses base 0 (bit position == VLAN id; 2k/3k/4k columns use
// bases 1024/2048/3072), Q-BRIDGE PortList uses base 1 (first bit == port 1).
pub(crate) fn bitmap_values(octets: &[u8], base: i64) -> Vec<i64> {
    let mut values = Vec::new();
    for (i, octet) in octets.iter().enumerate() {
        for j in 0..8u32 {
            if octet & (0x80u8 >> j) != 0 {
                values.push(base + (i as i64) * 8 + j as i64);
            }
        }
    }
    values
}

fn obj_bitmap(entry: &crate::collectors::poller::SNMPBotResultEntry, key: &str, base: i64) -> Vec<i64> {
    match obj_str(&entry.objects, key) {
        Some(hex) => bitmap_values(&parse_hex_octets(&hex), base),
        None => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Cisco decode (vlanTrunkPortTable + vtpVlanTable + vmMembershipTable)
// ---------------------------------------------------------------------------

// VLANs that actually exist on the device: operational ethernet rows of
// vtpVlanTable (skips suspended VLANs and the internal fddi/tokenRing 1002-1005
// defaults). Trunk allowed-VLAN bitmaps are intersected with this set because
// a default "switchport trunk allowed vlan all" reports all 4096 bits.
fn active_vlans(vtp_vlans: &SNMPBotResponse) -> HashSet<i64> {
    let mut active = HashSet::new();
    for entry in vtp_vlans.entries.iter() {
        let vlan = match entry.index.get("CISCO-VTP-MIB::vtpVlanIndex") {
            Some(v) => *v,
            None => continue,
        };
        let state = obj_str(&entry.objects, "CISCO-VTP-MIB::vtpVlanState").unwrap_or_default();
        let vlan_type = obj_str(&entry.objects, "CISCO-VTP-MIB::vtpVlanType").unwrap_or_default();
        if state == "operational" && vlan_type == "ethernet" {
            active.insert(vlan);
        }
    }
    active
}

pub(crate) fn decode_cisco(
    trunk: &SNMPBotResponse,
    vtp_vlans: Option<&SNMPBotResponse>,
    membership: Option<&SNMPBotResponse>,
) -> HashMap<i64, InterfaceVlans> {
    // Without vtpVlanTable the allowed-VLAN bitmaps cannot be bounded, so
    // trunks degrade to native-only rather than reporting ~4094 tagged VLANs.
    let active = vtp_vlans.map(active_vlans).unwrap_or_default();

    let mut access: HashMap<i64, i64> = HashMap::new();
    for entry in membership.map(|m| m.entries.iter()).into_iter().flatten() {
        let ifindex = match entry.index.get("IF-MIB::ifIndex") {
            Some(v) => *v,
            None => continue,
        };
        if let Some(vlan) = obj_i64(&entry.objects, "CISCO-VLAN-MEMBERSHIP-MIB::vmVlan") {
            if vlan > 0 {
                access.insert(ifindex, vlan);
            }
        }
    }

    let mut result: HashMap<i64, InterfaceVlans> = HashMap::new();
    for entry in trunk.entries.iter() {
        let ifindex = match entry.index.get("CISCO-VTP-MIB::vlanTrunkPortIfIndex") {
            Some(v) => *v,
            None => continue,
        };
        let status = obj_str(&entry.objects, "CISCO-VTP-MIB::vlanTrunkPortDynamicStatus").unwrap_or_default();
        if status == "trunking" {
            // vlanTrunkPortNativeVlan is 0 on rows without a real native VLAN
            // (e.g. port-channel members).
            let native = match obj_i64(&entry.objects, "CISCO-VTP-MIB::vlanTrunkPortNativeVlan") {
                Some(v) if v > 0 => Some(v),
                _ => None,
            };
            let mut enabled: Vec<i64> = Vec::new();
            enabled.extend(obj_bitmap(entry, "CISCO-VTP-MIB::vlanTrunkPortVlansEnabled", 0));
            enabled.extend(obj_bitmap(entry, "CISCO-VTP-MIB::vlanTrunkPortVlansEnabled2k", 1024));
            enabled.extend(obj_bitmap(entry, "CISCO-VTP-MIB::vlanTrunkPortVlansEnabled3k", 2048));
            enabled.extend(obj_bitmap(entry, "CISCO-VTP-MIB::vlanTrunkPortVlansEnabled4k", 3072));
            let tagged: Vec<i64> = enabled
                .into_iter()
                .filter(|v| active.contains(v) && Some(*v) != native)
                .collect();
            result.insert(ifindex, InterfaceVlans { native_vlan: native, tagged_vlans: tagged });
        } else if let Some(vlan) = access.get(&ifindex) {
            // Access port: vmVlan is authoritative (the trunk row still carries
            // a *configured* native VLAN, which is not the access VLAN). Ports
            // absent from vmMembershipTable (e.g. routed ports) get no entry.
            result.insert(ifindex, InterfaceVlans { native_vlan: Some(*vlan), tagged_vlans: Vec::new() });
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Q-BRIDGE fallback decode (dot1qPortVlanTable + dot1qVlanCurrent/StaticTable)
// ---------------------------------------------------------------------------

pub(crate) fn decode_qbridge(
    pvid: Option<&SNMPBotResponse>,
    vlan_ports: Option<&SNMPBotResponse>,
    base_ports: &SNMPBotResponse,
) -> HashMap<i64, InterfaceVlans> {
    // bridge port -> real ifIndex (same translation as entitypoller STP).
    let mut ifindex_by_bridge_port: HashMap<i64, i64> = HashMap::new();
    for entry in base_ports.entries.iter() {
        let bridge_port = match entry.index.get("BRIDGE-MIB::dot1dBasePort") {
            Some(v) => *v,
            None => continue,
        };
        if let Some(ifindex) = obj_i64(&entry.objects, "BRIDGE-MIB::dot1dBasePortIfIndex") {
            ifindex_by_bridge_port.insert(bridge_port, ifindex);
        }
    }

    let mut native: HashMap<i64, i64> = HashMap::new();
    for entry in pvid.map(|p| p.entries.iter()).into_iter().flatten() {
        // dot1qPortVlanTable AUGMENTS dot1dBasePortEntry, so snmpbot indexes
        // its rows by the BRIDGE-MIB name.
        let bridge_port = match entry.index.get("BRIDGE-MIB::dot1dBasePort") {
            Some(v) => *v,
            None => continue,
        };
        let ifindex = match ifindex_by_bridge_port.get(&bridge_port) {
            Some(v) => *v,
            None => continue,
        };
        if let Some(vlan) = obj_i64(&entry.objects, "Q-BRIDGE-MIB::dot1qPvid") {
            if vlan > 0 {
                native.insert(ifindex, vlan);
            }
        }
    }

    // Per VLAN row: tagged ports = egress PortList minus untagged PortList.
    // dot1qVlanCurrentTable's index also has dot1qVlanTimeMark; keying rows by
    // dot1qVlanIndex only (with a set per port) deduplicates snapshot rows.
    let mut tagged: HashMap<i64, BTreeSet<i64>> = HashMap::new();
    for entry in vlan_ports.map(|v| v.entries.iter()).into_iter().flatten() {
        let vlan = match entry.index.get("Q-BRIDGE-MIB::dot1qVlanIndex") {
            Some(v) => *v,
            None => continue,
        };
        let egress = obj_bitmap(entry, "Q-BRIDGE-MIB::dot1qVlanCurrentEgressPorts", 1);
        let egress = if egress.is_empty() { obj_bitmap(entry, "Q-BRIDGE-MIB::dot1qVlanStaticEgressPorts", 1) } else { egress };
        let untagged_current = obj_bitmap(entry, "Q-BRIDGE-MIB::dot1qVlanCurrentUntaggedPorts", 1);
        let untagged: HashSet<i64> = if untagged_current.is_empty() {
            obj_bitmap(entry, "Q-BRIDGE-MIB::dot1qVlanStaticUntaggedPorts", 1).into_iter().collect()
        } else {
            untagged_current.into_iter().collect()
        };
        for bridge_port in egress.into_iter().filter(|p| !untagged.contains(p)) {
            if let Some(ifindex) = ifindex_by_bridge_port.get(&bridge_port) {
                tagged.entry(*ifindex).or_insert_with(BTreeSet::new).insert(vlan);
            }
        }
    }

    let mut result: HashMap<i64, InterfaceVlans> = HashMap::new();
    for (ifindex, vlan) in native.iter() {
        result.insert(*ifindex, InterfaceVlans { native_vlan: Some(*vlan), tagged_vlans: Vec::new() });
    }
    for (ifindex, vlans) in tagged.into_iter() {
        let entry = result.entry(ifindex).or_insert_with(|| InterfaceVlans { native_vlan: None, tagged_vlans: Vec::new() });
        entry.tagged_vlans = vlans.into_iter().filter(|v| Some(*v) != entry.native_vlan).collect();
    }
    result
}

// ---------------------------------------------------------------------------
// Per-device poll
// ---------------------------------------------------------------------------

fn poll_device(snmpbot_url: &String, fqdn: &String, community: &String) -> Option<HashMap<i64, InterfaceVlans>> {
    let host = format!("{}@{}", community, fqdn);

    // Cisco first: the trunk table exists (with rows) on every Cisco switch.
    // jaspyVlanTrunkPortTable is a jaspy-specific slim view of
    // vlanTrunkPortTable (snmpbot/mibs/CISCO-VTP-MIB.json) holding only the 6
    // columns we decode: walking the full ~30-column entry (a dozen 128-byte
    // PortList octet strings per row) silently truncates on slow switches
    // (verified on a C2960CX, where the full walk stopped 5 rows short).
    if let Some(trunk) = fetch_table(snmpbot_url, &host, "CISCO-VTP-MIB::jaspyVlanTrunkPortTable") {
        if !trunk.entries.is_empty() {
            let vtp_vlans = fetch_table(snmpbot_url, &host, "CISCO-VTP-MIB::vtpVlanTable");
            if vtp_vlans.is_none() {
                println!("[vlanpoller] [{}] vtpVlanTable unavailable; trunk tagged VLANs degrade to native-only", host);
            }
            let membership = fetch_table(snmpbot_url, &host, "CISCO-VLAN-MEMBERSHIP-MIB::vmMembershipTable");
            let decoded = decode_cisco(&trunk, vtp_vlans.as_ref(), membership.as_ref());
            if !decoded.is_empty() {
                return Some(decoded);
            }
        }
    }

    // Q-BRIDGE fallback (standards-based; not supported by Cisco IOS switches,
    // which never reach this point because the trunk table decoded above).
    let base_ports = fetch_table(snmpbot_url, &host, "BRIDGE-MIB::dot1dBasePortTable")?;
    let pvid = fetch_table(snmpbot_url, &host, "Q-BRIDGE-MIB::dot1qPortVlanTable");
    let vlan_ports = match fetch_table(snmpbot_url, &host, "Q-BRIDGE-MIB::dot1qVlanCurrentTable") {
        Some(current) if !current.entries.is_empty() => Some(current),
        _ => fetch_table(snmpbot_url, &host, "Q-BRIDGE-MIB::dot1qVlanStaticTable"),
    };
    let decoded = decode_qbridge(pvid.as_ref(), vlan_ports.as_ref(), &base_ports);
    if decoded.is_empty() { None } else { Some(decoded) }
}

// ---------------------------------------------------------------------------
// Supervisor
// ---------------------------------------------------------------------------

fn load_devices(pool: &db::Pool) -> Vec<(String, String)> {
    let mut devices: Vec<(String, String)> = Vec::new();
    if let Ok(mut conn) = pool.get() {
        for device in crate::models::dbo::Device::monitored(&mut *conn).iter() {
            let community = match device.snmp_community {
                Some(ref c) => c.clone(),
                None => continue,
            };
            devices.push((format!("{}.{}", device.name, device.dns_domain), community));
        }
    } else {
        println!("[vlanpoller] failed to acquire db connection for device listing");
    }
    devices
}

fn poll_batch(snmpbot_url: &String, devices: Vec<(String, String)>, store: &Arc<Mutex<VlanStore>>, jitter_msecs: u64) {
    let mut handles = Vec::new();
    for (fqdn, community) in devices.into_iter() {
        let snmpbot_url = snmpbot_url.clone();
        let store = store.clone();
        handles.push(thread::spawn(move || {
            if jitter_msecs > 0 {
                let sleep = thread_rng().gen_range(0.0, jitter_msecs as f64);
                thread::sleep(time::Duration::from_millis(sleep as u64));
            }
            // Only replace on success so a transient poll failure does not
            // blank previously known VLAN data.
            if let Some(interfaces) = poll_device(&snmpbot_url, &fqdn, &community) {
                if let Ok(mut store) = store.lock() {
                    store.replace_device(fqdn, interfaces);
                }
            }
        }));
    }
    for handle in handles {
        let _ = handle.join();
    }
}

pub fn run(
    snmpbot_url: String,
    interval_msecs: u64,
    control: Arc<Mutex<VlanPollerControl>>,
    store: Arc<Mutex<VlanStore>>,
    running: Arc<atomic::AtomicBool>,
) {
    println!("[vlanpoller] starting in-process collector (snmpbot={}, interval_msecs={})", snmpbot_url, interval_msecs);
    let pool = db::connect();
    let no_jitter = std::env::var("JASPY_POLLER_NO_JITTER").map(|v| v == "1" || v == "true").unwrap_or(false);
    let mut next_cycle: u64 = 0; // first full cycle runs immediately

    while running.load(atomic::Ordering::Relaxed) {
        let now = tools::get_time_msecs();
        if now >= next_cycle {
            let devices = load_devices(&pool);
            let keep: HashSet<String> = devices.iter().map(|(fqdn, _)| fqdn.clone()).collect();
            if let Ok(mut store) = store.lock() {
                store.retain(&keep);
            }
            let jitter = if no_jitter { 0 } else { interval_msecs / 2 };
            poll_batch(&snmpbot_url, devices, &store, jitter);
            next_cycle = tools::get_time_msecs() + interval_msecs;
        }

        // Poll-now queue: poll the requested devices immediately (no jitter),
        // then release them from `pending` so the route stops answering 409.
        let triggered: HashSet<String> = match control.lock() {
            Ok(control) => control.pending.iter().cloned().collect(),
            Err(_) => HashSet::new(),
        };
        if !triggered.is_empty() {
            let devices: Vec<(String, String)> = load_devices(&pool)
                .into_iter()
                .filter(|(fqdn, _)| triggered.contains(fqdn))
                .collect();
            poll_batch(&snmpbot_url, devices, &store, 0);
            if let Ok(mut control) = control.lock() {
                control.pending.retain(|fqdn| !triggered.contains(fqdn));
            }
        }

        interruptible_sleep(1000, &running);
    }
    println!("[vlanpoller] collector stopped");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const TRUNK: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/vlantrunkporttable.json"));
    const VTP_VLANS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/vtpvlantable.json"));
    const MEMBERSHIP: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/vmmembershiptable.json"));
    const PVID: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dot1qportvlantable.json"));
    const VLAN_CURRENT: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dot1qvlancurrenttable.json"));
    const BASE_PORTS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dot1dbaseporttable.json"));

    fn parse(fixture: &str) -> SNMPBotResponse {
        serde_json::from_str(fixture).unwrap()
    }

    // --- parse_hex_octets ---

    #[test]
    fn hex_octets_parses_snmpbot_format() {
        assert_eq!(parse_hex_octets("7f ff 00"), vec![0x7f, 0xff, 0x00]);
        assert_eq!(parse_hex_octets(""), Vec::<u8>::new());
        // Uppercase tolerated even though snmpbot emits lowercase.
        assert_eq!(parse_hex_octets("FF 0A"), vec![0xff, 0x0a]);
    }

    #[test]
    fn hex_octets_garbage_voids_value() {
        assert_eq!(parse_hex_octets("7f zz ff"), Vec::<u8>::new());
        assert_eq!(parse_hex_octets("7f 100"), Vec::<u8>::new());
    }

    // --- bitmap_values ---

    #[test]
    fn bitmap_is_msb_first() {
        // 0x80 = first bit of first octet = base + 0.
        assert_eq!(bitmap_values(&[0x80], 0), vec![0]);
        assert_eq!(bitmap_values(&[0x80], 1), vec![1]);
        // 0x7f = bits 1..7 of the first octet (real ticket-sw2 first octet).
        assert_eq!(bitmap_values(&[0x7f], 0), vec![1, 2, 3, 4, 5, 6, 7]);
        // Second octet starts at base + 8.
        assert_eq!(bitmap_values(&[0x00, 0x20], 0), vec![10]);
        // Cisco 2k column: VLAN 1024 is the first bit.
        assert_eq!(bitmap_values(&[0x80], 1024), vec![1024]);
        // PortList: ports 5 and 6 are bits 4 and 5 (base 1).
        assert_eq!(bitmap_values(&[0x0c], 1), vec![5, 6]);
    }

    // --- decode_cisco (fixtures modeled on live ticket-sw2 data) ---

    #[test]
    fn cisco_trunk_intersects_enabled_bitmap_with_active_vlans() {
        let decoded = decode_cisco(&parse(TRUNK), Some(&parse(VTP_VLANS)), Some(&parse(MEMBERSHIP)));
        // Trunk with all-0xFF allowed bitmaps: tagged collapses to the
        // operational ethernet VLANs {1, 300, 311} minus native 300.
        // Suspended VLAN 400 and fddi VLAN 1002 are excluded.
        let trunk = &decoded[&10101];
        assert_eq!(trunk.native_vlan, Some(300));
        assert_eq!(trunk.tagged_vlans, vec![1, 311]);
    }

    #[test]
    fn cisco_access_port_uses_vm_vlan_not_trunk_native() {
        let decoded = decode_cisco(&parse(TRUNK), Some(&parse(VTP_VLANS)), Some(&parse(MEMBERSHIP)));
        // notTrunking port: native comes from vmMembershipTable (311), NOT the
        // trunk row's configured native VLAN (1); no tagged VLANs.
        let access = &decoded[&10102];
        assert_eq!(access.native_vlan, Some(311));
        assert!(access.tagged_vlans.is_empty());
    }

    #[test]
    fn cisco_native_vlan_zero_is_none() {
        let decoded = decode_cisco(&parse(TRUNK), Some(&parse(VTP_VLANS)), Some(&parse(MEMBERSHIP)));
        // Port-channel-style row: trunking with native VLAN 0.
        let po = &decoded[&5001];
        assert_eq!(po.native_vlan, None);
        assert_eq!(po.tagged_vlans, vec![1, 300, 311]);
    }

    #[test]
    fn cisco_access_port_without_membership_row_is_absent() {
        let mut membership = parse(MEMBERSHIP);
        membership.entries.clear();
        let decoded = decode_cisco(&parse(TRUNK), Some(&parse(VTP_VLANS)), Some(&membership));
        assert!(!decoded.contains_key(&10102));
        // Trunks are unaffected.
        assert!(decoded.contains_key(&10101));
        let decoded = decode_cisco(&parse(TRUNK), Some(&parse(VTP_VLANS)), None);
        assert!(!decoded.contains_key(&10102));
    }

    #[test]
    fn cisco_missing_vtp_vlan_table_degrades_to_native_only() {
        let decoded = decode_cisco(&parse(TRUNK), None, Some(&parse(MEMBERSHIP)));
        let trunk = &decoded[&10101];
        assert_eq!(trunk.native_vlan, Some(300));
        // Without the active-VLAN set the all-0xFF bitmap must NOT expand to
        // ~4094 tagged VLANs.
        assert!(trunk.tagged_vlans.is_empty());
    }

    // --- decode_qbridge ---

    #[test]
    fn qbridge_decodes_pvid_and_tagged_via_bridge_port_translation() {
        let decoded = decode_qbridge(Some(&parse(PVID)), Some(&parse(VLAN_CURRENT)), &parse(BASE_PORTS));
        // Bridge port 5 -> ifIndex 10101 (dot1dbaseporttable.json). PVID 10;
        // VLAN 20's egress-minus-untagged contains port 5 -> tagged [20].
        // VLAN 10's tagged port 6 has no ifIndex mapping and is dropped, as is
        // port 6's PVID row.
        assert_eq!(decoded.len(), 1);
        let port = &decoded[&10101];
        assert_eq!(port.native_vlan, Some(10));
        assert_eq!(port.tagged_vlans, vec![20]);
    }

    #[test]
    fn qbridge_time_mark_duplicate_rows_dedupe() {
        // The fixture carries VLAN 10 twice (TimeMark 0 and 100); the per-port
        // BTreeSet keyed by dot1qVlanIndex must not double anything.
        let decoded = decode_qbridge(Some(&parse(PVID)), Some(&parse(VLAN_CURRENT)), &parse(BASE_PORTS));
        assert_eq!(decoded[&10101].tagged_vlans, vec![20]);
    }

    #[test]
    fn qbridge_static_column_names_also_decode() {
        let vlan_static = r#"{
            "ID": "Q-BRIDGE-MIB::dot1qVlanStaticTable",
            "IndexKeys": ["Q-BRIDGE-MIB::dot1qVlanIndex"],
            "ObjectKeys": ["Q-BRIDGE-MIB::dot1qVlanStaticEgressPorts", "Q-BRIDGE-MIB::dot1qVlanStaticUntaggedPorts"],
            "Entries": [{
                "HostID": "sw1.test.example",
                "Index": {"Q-BRIDGE-MIB::dot1qVlanIndex": 30},
                "Objects": {
                    "Q-BRIDGE-MIB::dot1qVlanStaticEgressPorts": "08 00 00 00",
                    "Q-BRIDGE-MIB::dot1qVlanStaticUntaggedPorts": "00 00 00 00"
                }
            }]
        }"#;
        let decoded = decode_qbridge(None, Some(&parse(vlan_static)), &parse(BASE_PORTS));
        assert_eq!(decoded[&10101].native_vlan, None);
        assert_eq!(decoded[&10101].tagged_vlans, vec![30]);
    }

    #[test]
    fn qbridge_native_vlan_excluded_from_tagged() {
        // A port whose PVID also shows up as tagged (untagged bitmap not
        // reported) must not list its native VLAN as tagged.
        let vlan_current = r#"{
            "ID": "Q-BRIDGE-MIB::dot1qVlanCurrentTable",
            "IndexKeys": ["Q-BRIDGE-MIB::dot1qVlanTimeMark", "Q-BRIDGE-MIB::dot1qVlanIndex"],
            "ObjectKeys": ["Q-BRIDGE-MIB::dot1qVlanCurrentEgressPorts", "Q-BRIDGE-MIB::dot1qVlanCurrentUntaggedPorts"],
            "Entries": [{
                "HostID": "sw1.test.example",
                "Index": {"Q-BRIDGE-MIB::dot1qVlanTimeMark": 0, "Q-BRIDGE-MIB::dot1qVlanIndex": 10},
                "Objects": {
                    "Q-BRIDGE-MIB::dot1qVlanCurrentEgressPorts": "08 00 00 00",
                    "Q-BRIDGE-MIB::dot1qVlanCurrentUntaggedPorts": "00 00 00 00"
                }
            }]
        }"#;
        let decoded = decode_qbridge(Some(&parse(PVID)), Some(&parse(vlan_current)), &parse(BASE_PORTS));
        let port = &decoded[&10101];
        assert_eq!(port.native_vlan, Some(10));
        assert!(port.tagged_vlans.is_empty());
    }

    // --- VlanStore ---

    fn vlans(native: Option<i64>, tagged: &[i64]) -> HashMap<i64, InterfaceVlans> {
        let mut m = HashMap::new();
        m.insert(10101, InterfaceVlans { native_vlan: native, tagged_vlans: tagged.to_vec() });
        m
    }

    #[test]
    fn store_replace_and_snapshot() {
        let mut store = VlanStore::new();
        store.replace_device("sw1.example.com".to_string(), vlans(Some(300), &[1, 311]));
        let snapshot = store.device_vlans("sw1.example.com");
        assert_eq!(snapshot[&10101].native_vlan, Some(300));
        assert_eq!(snapshot[&10101].tagged_vlans, vec![1, 311]);

        store.replace_device("sw1.example.com".to_string(), vlans(Some(1), &[]));
        assert_eq!(store.device_vlans("sw1.example.com")[&10101].native_vlan, Some(1));
    }

    #[test]
    fn store_unknown_fqdn_is_empty() {
        assert!(VlanStore::new().device_vlans("ghost.example.com").is_empty());
    }

    #[test]
    fn store_retain_drops_unmonitored_devices() {
        let mut store = VlanStore::new();
        store.replace_device("keep.example.com".to_string(), vlans(Some(1), &[]));
        store.replace_device("drop.example.com".to_string(), vlans(Some(2), &[]));
        let keep: HashSet<String> = vec!["keep.example.com".to_string()].into_iter().collect();
        store.retain(&keep);
        assert!(!store.device_vlans("keep.example.com").is_empty());
        assert!(store.device_vlans("drop.example.com").is_empty());
    }
}
