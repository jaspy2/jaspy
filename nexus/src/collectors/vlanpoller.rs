// In-process per-interface VLAN membership collector: native (untagged) VLAN
// and tagged VLAN list per port.
//
// One vendor::Source per incompatible MIB family (probe order and per-device
// winner cache in collectors::vendor):
//   - CiscoVtpSource: CISCO-VTP-MIB::vlanTrunkPortTable (trunk native +
//     allowed-VLAN bitmaps, intersected with the VLANs that actually exist
//     per CISCO-VTP-MIB::vtpVlanTable — default trunks report all 4096 bits
//     set) plus CISCO-VLAN-MEMBERSHIP-MIB::vmMembershipTable (access-port
//     VLAN).
//   - QBridgeSource: standard Q-BRIDGE-MIB: dot1qPvid per bridge port and
//     dot1qVlanCurrentTable (or dot1qVlanStaticTable) egress/untagged
//     PortList bitmaps, translated to ifIndex via
//     BRIDGE-MIB::dot1dBasePortTable.
//
// Results live only in the in-memory `VlanStore` (no DB, no Prometheus): the
// data is re-polled on an interval and can also be refreshed on demand per
// device via POST /api/v1/devices/<fqdn>/vlans/poll, which queues the fqdn in
// `VlanPollerControl` for the supervisor loop to pick up on its next 1s tick.
extern crate serde_json;

use crate::collectors::entitypoller::{fetch_table, interruptible_sleep, obj_i64, obj_str};
use crate::collectors::poller::SNMPBotResponse;
use crate::collectors::vendor::{self, Vendor};
use crate::snmp::SnmpSource;
use crate::db;
use crate::utilities::tools;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{atomic, Arc, Mutex};

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

// One device's poll result: per-ifIndex membership plus the device's VLAN
// id -> name map (vtpVlanName / dot1qVlanStaticName).
#[derive(Clone, Default)]
pub struct DeviceVlans {
    pub interfaces: HashMap<i64, InterfaceVlans>, // keyed by ifIndex
    pub names: HashMap<i64, String>,              // keyed by VLAN id
}

pub struct VlanStore {
    devices: HashMap<String, DeviceVlans>, // keyed by fqdn
}

impl VlanStore {
    pub fn new() -> VlanStore {
        VlanStore { devices: HashMap::new() }
    }

    fn replace_device(&mut self, fqdn: String, vlans: DeviceVlans) {
        self.devices.insert(fqdn, vlans);
    }

    fn retain(&mut self, keep: &HashSet<String>) {
        self.devices.retain(|fqdn, _| keep.contains(fqdn));
    }

    // Snapshot for the API route; unknown fqdn and not-yet-polled both empty.
    pub fn device_vlans(&self, fqdn: &str) -> DeviceVlans {
        self.devices.get(fqdn).cloned().unwrap_or_default()
    }

    // Per-device, per-ifIndex membership as (native, tagged) tuples — the shape
    // the STP tree builder needs to test whether a link carries a VLAN, without
    // coupling it to this collector's types.
    pub fn membership_map(&self) -> HashMap<String, HashMap<i64, (Option<i64>, Vec<i64>)>> {
        self.devices.iter().map(|(fqdn, dev)| {
            let ifaces = dev.interfaces.iter()
                .map(|(ifindex, v)| (*ifindex, (v.native_vlan, v.tagged_vlans.clone())))
                .collect();
            (fqdn.clone(), ifaces)
        }).collect()
    }

    // Network-wide inventory for GET /api/v1/vlans: every VLAN known on any
    // device (named or referenced by a port), with the per-device name and
    // port usage. `names` collects the distinct names across devices — more
    // than one entry means the switches disagree about the VLAN's name.
    pub fn network_vlans(&self) -> Vec<crate::models::json::ApiVlanSummary> {
        use crate::models::json::{ApiVlanDevice, ApiVlanSummary};
        let mut by_id: std::collections::BTreeMap<i64, Vec<ApiVlanDevice>> = std::collections::BTreeMap::new();
        for (fqdn, device) in self.devices.iter() {
            let mut ids: std::collections::BTreeSet<i64> = device.names.keys().cloned().collect();
            for interface in device.interfaces.values() {
                ids.extend(interface.native_vlan.iter());
                ids.extend(interface.tagged_vlans.iter());
            }
            for id in ids {
                by_id.entry(id).or_default().push(ApiVlanDevice {
                    fqdn: fqdn.clone(),
                    name: device.names.get(&id).cloned(),
                    native_ports: device.interfaces.values().filter(|i| i.native_vlan == Some(id)).count() as i64,
                    tagged_ports: device.interfaces.values().filter(|i| i.tagged_vlans.contains(&id)).count() as i64,
                });
            }
        }
        by_id.into_iter().map(|(id, mut devices)| {
            devices.sort_by(|a, b| a.fqdn.cmp(&b.fqdn));
            let mut names: Vec<String> = devices.iter().filter_map(|d| d.name.clone()).collect();
            names.sort();
            names.dedup();
            ApiVlanSummary { id, names, devices }
        }).collect()
    }

    // Distinct, sorted names per VLAN id across every device — the id -> name
    // resolver for views keyed by VLAN number (STP page, STP issues). One name
    // means the network agrees; more than one means the switches disagree (same
    // conflict model as network_vlans / the Vlans page). Empty names are skipped.
    pub fn vlan_names(&self) -> HashMap<i64, Vec<String>> {
        let mut by_id: HashMap<i64, std::collections::BTreeSet<String>> = HashMap::new();
        for device in self.devices.values() {
            for (id, name) in device.names.iter() {
                if !name.is_empty() {
                    by_id.entry(*id).or_default().insert(name.clone());
                }
            }
        }
        by_id.into_iter().map(|(id, names)| (id, names.into_iter().collect())).collect()
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

// VLAN id -> name from vtpVlanTable ethernet rows (any state, so an access
// port parked on a suspended VLAN still resolves its name).
pub(crate) fn cisco_vlan_names(vtp_vlans: &SNMPBotResponse) -> HashMap<i64, String> {
    let mut names = HashMap::new();
    for entry in vtp_vlans.entries.iter() {
        let vlan = match entry.index.get("CISCO-VTP-MIB::vtpVlanIndex") {
            Some(v) => *v,
            None => continue,
        };
        if obj_str(&entry.objects, "CISCO-VTP-MIB::vtpVlanType").as_deref() != Some("ethernet") {
            continue;
        }
        if let Some(name) = obj_str(&entry.objects, "CISCO-VTP-MIB::vtpVlanName") {
            if !name.is_empty() {
                names.insert(vlan, name);
            }
        }
    }
    names
}

// VLAN id -> name from dot1qVlanStaticTable (the Current table has no name).
pub(crate) fn qbridge_vlan_names(vlan_static: &SNMPBotResponse) -> HashMap<i64, String> {
    let mut names = HashMap::new();
    for entry in vlan_static.entries.iter() {
        let vlan = match entry.index.get("Q-BRIDGE-MIB::dot1qVlanIndex") {
            Some(v) => *v,
            None => continue,
        };
        if let Some(name) = obj_str(&entry.objects, "Q-BRIDGE-MIB::dot1qVlanStaticName") {
            if !name.is_empty() {
                names.insert(vlan, name);
            }
        }
    }
    names
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

struct VlanCtx<'a> {
    snmp: &'a SnmpSource,
    host: &'a String,
}

struct CiscoVtpSource;

impl<'a> vendor::Source<VlanCtx<'a>> for CiscoVtpSource {
    type Output = DeviceVlans;

    fn name(&self) -> &'static str {
        "cisco-vtp"
    }

    fn vendor(&self) -> Vendor {
        Vendor::Cisco
    }

    // The trunk table exists (with rows) on every Cisco switch.
    // jaspyVlanTrunkPortTable is a jaspy-specific slim view of
    // vlanTrunkPortTable (snmpbot/mibs/CISCO-VTP-MIB.json) holding only the 6
    // columns we decode: walking the full ~30-column entry (a dozen 128-byte
    // PortList octet strings per row) silently truncates on slow switches
    // (verified on a C2960CX, where the full walk stopped 5 rows short).
    fn collect(&self, ctx: &VlanCtx) -> Option<DeviceVlans> {
        let trunk = match fetch_table(ctx.snmp, ctx.host, "CISCO-VTP-MIB::jaspyVlanTrunkPortTable") {
            Some(t) if !t.entries.is_empty() => t,
            _ => return None,
        };
        let vtp_vlans = fetch_table(ctx.snmp, ctx.host, "CISCO-VTP-MIB::vtpVlanTable");
        if vtp_vlans.is_none() {
            println!("[vlanpoller] [{}] vtpVlanTable unavailable; trunk tagged VLANs degrade to native-only", ctx.host);
        }
        let membership = fetch_table(ctx.snmp, ctx.host, "CISCO-VLAN-MEMBERSHIP-MIB::vmMembershipTable");
        let decoded = decode_cisco(&trunk, vtp_vlans.as_ref(), membership.as_ref());
        if decoded.is_empty() {
            // A trunk table with rows but nothing decoded (e.g. all ports
            // routed): report nothing so selection can try the next source.
            return None;
        }
        Some(DeviceVlans {
            interfaces: decoded,
            names: vtp_vlans.as_ref().map(cisco_vlan_names).unwrap_or_default(),
        })
    }
}

struct QBridgeSource;

impl<'a> vendor::Source<VlanCtx<'a>> for QBridgeSource {
    type Output = DeviceVlans;

    fn name(&self) -> &'static str {
        "q-bridge"
    }

    fn vendor(&self) -> Vendor {
        Vendor::Generic
    }

    // Standards-based; Cisco IOS switches don't answer Q-BRIDGE, so they only
    // reach these probes when the Cisco source declined (which its trunk
    // table usually prevents).
    fn collect(&self, ctx: &VlanCtx) -> Option<DeviceVlans> {
        let base_ports = fetch_table(ctx.snmp, ctx.host, "BRIDGE-MIB::dot1dBasePortTable")?;
        let pvid = fetch_table(ctx.snmp, ctx.host, "Q-BRIDGE-MIB::dot1qPortVlanTable");
        // The Static table is fetched regardless: it is both the port-membership
        // fallback and the only source of VLAN names in Q-BRIDGE-MIB.
        let vlan_static = fetch_table(ctx.snmp, ctx.host, "Q-BRIDGE-MIB::dot1qVlanStaticTable");
        let vlan_ports = match fetch_table(ctx.snmp, ctx.host, "Q-BRIDGE-MIB::dot1qVlanCurrentTable") {
            Some(current) if !current.entries.is_empty() => Some(current),
            _ => vlan_static.clone(),
        };
        let decoded = decode_qbridge(pvid.as_ref(), vlan_ports.as_ref(), &base_ports);
        if decoded.is_empty() {
            return None;
        }
        Some(DeviceVlans {
            interfaces: decoded,
            names: vlan_static.as_ref().map(qbridge_vlan_names).unwrap_or_default(),
        })
    }
}

fn poll_device(snmp: &SnmpSource, fqdn: &String, community: &String, hint: Vendor, sources_cache: &vendor::SourceCache) -> Option<DeviceVlans> {
    let host = format!("{}@{}", community, fqdn);
    let ctx = VlanCtx { snmp: snmp, host: &host };
    let sources: [&dyn vendor::Source<VlanCtx, Output = DeviceVlans>; 2] = [&CiscoVtpSource, &QBridgeSource];
    vendor::collect_first(sources_cache, fqdn, hint, &sources, &ctx)
}

// ---------------------------------------------------------------------------
// Supervisor
// ---------------------------------------------------------------------------

struct VlanDevice {
    fqdn: String,
    community: String,
    // Seeds the source probe order (see collectors::vendor).
    vendor: Vendor,
}

fn load_devices(pool: &db::Pool) -> Vec<VlanDevice> {
    let mut devices: Vec<VlanDevice> = Vec::new();
    if let Ok(mut conn) = pool.get() {
        for device in crate::models::dbo::Device::monitored(&mut *conn).iter() {
            let community = match device.snmp_community {
                Some(ref c) => c.clone(),
                None => continue,
            };
            devices.push(VlanDevice {
                fqdn: format!("{}.{}", device.name, device.dns_domain),
                community: community,
                vendor: vendor::vendor_hint(device.os_info.as_deref(), device.device_type.as_deref()),
            });
        }
    } else {
        println!("[vlanpoller] failed to acquire db connection for device listing");
    }
    devices
}

// Cap on simultaneous per-device poll threads (each holds a blocking HTTP
// connection to snmpbot for its device's whole table sequence).
const MAX_POLL_WORKERS: usize = 16;

fn poll_batch(
    snmp: &SnmpSource,
    devices: Vec<VlanDevice>,
    store: &Arc<Mutex<VlanStore>>,
    sources_cache: &Arc<vendor::SourceCache>,
    jitter_msecs: u64,
) {
    crate::collectors::pool::run_bounded(devices, MAX_POLL_WORKERS, jitter_msecs, |device| {
        // Only replace on success so a transient poll failure does not
        // blank previously known VLAN data.
        if let Some(vlans) = poll_device(snmp, &device.fqdn, &device.community, device.vendor, sources_cache) {
            if let Ok(mut store) = store.lock() {
                store.replace_device(device.fqdn, vlans);
            }
        }
    });
}

pub fn run(
    snmp: Arc<SnmpSource>,
    interval_msecs: u64,
    control: Arc<Mutex<VlanPollerControl>>,
    store: Arc<Mutex<VlanStore>>,
    running: Arc<atomic::AtomicBool>,
) {
    println!("[vlanpoller] starting in-process collector (interval_msecs={})", interval_msecs);
    let pool = db::connect();
    let no_jitter = std::env::var("JASPY_POLLER_NO_JITTER").map(|v| v == "1" || v == "true").unwrap_or(false);
    let sources_cache = Arc::new(vendor::SourceCache::new());
    let mut next_cycle: u64 = 0; // first full cycle runs immediately

    while running.load(atomic::Ordering::Relaxed) {
        let now = tools::get_time_msecs();
        if now >= next_cycle {
            let devices = load_devices(&pool);
            let keep: HashSet<String> = devices.iter().map(|d| d.fqdn.clone()).collect();
            if let Ok(mut store) = store.lock() {
                store.retain(&keep);
            }
            sources_cache.retain(&keep);
            let jitter = if no_jitter { 0 } else { interval_msecs / 2 };
            poll_batch(&snmp, devices, &store, &sources_cache, jitter);
            next_cycle = tools::get_time_msecs() + interval_msecs;
        }

        // Poll-now queue: poll the requested devices immediately (no jitter),
        // then release them from `pending` so the route stops answering 409.
        let triggered: HashSet<String> = match control.lock() {
            Ok(control) => control.pending.iter().cloned().collect(),
            Err(_) => HashSet::new(),
        };
        if !triggered.is_empty() {
            let devices: Vec<VlanDevice> = load_devices(&pool)
                .into_iter()
                .filter(|d| triggered.contains(&d.fqdn))
                .collect();
            poll_batch(&snmp, devices, &store, &sources_cache, 0);
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

    // --- VLAN names ---

    #[test]
    fn cisco_names_come_from_ethernet_rows_of_any_state() {
        let names = cisco_vlan_names(&parse(VTP_VLANS));
        assert_eq!(names.get(&1).map(String::as_str), Some("default"));
        assert_eq!(names.get(&300).map(String::as_str), Some("Mgmt"));
        assert_eq!(names.get(&311).map(String::as_str), Some("Org"));
        // Suspended ethernet VLANs keep their name; fddi rows are excluded.
        assert_eq!(names.get(&400).map(String::as_str), Some("parked"));
        assert!(!names.contains_key(&1002));
    }

    #[test]
    fn qbridge_names_come_from_the_static_table() {
        let vlan_static = r#"{
            "ID": "Q-BRIDGE-MIB::dot1qVlanStaticTable",
            "IndexKeys": ["Q-BRIDGE-MIB::dot1qVlanIndex"],
            "ObjectKeys": ["Q-BRIDGE-MIB::dot1qVlanStaticName"],
            "Entries": [
                {"HostID": "sw1.test.example", "Index": {"Q-BRIDGE-MIB::dot1qVlanIndex": 10},
                 "Objects": {"Q-BRIDGE-MIB::dot1qVlanStaticName": "users"}},
                {"HostID": "sw1.test.example", "Index": {"Q-BRIDGE-MIB::dot1qVlanIndex": 20},
                 "Objects": {"Q-BRIDGE-MIB::dot1qVlanStaticName": ""}}
            ]
        }"#;
        let names = qbridge_vlan_names(&parse(vlan_static));
        assert_eq!(names.get(&10).map(String::as_str), Some("users"));
        // Empty names are omitted, not stored as "".
        assert!(!names.contains_key(&20));
    }

    // --- VlanStore ---

    fn vlans(native: Option<i64>, tagged: &[i64]) -> DeviceVlans {
        let mut interfaces = HashMap::new();
        interfaces.insert(10101, InterfaceVlans { native_vlan: native, tagged_vlans: tagged.to_vec() });
        let mut names = HashMap::new();
        names.insert(300, "Mgmt".to_string());
        DeviceVlans { interfaces, names }
    }

    #[test]
    fn store_replace_and_snapshot() {
        let mut store = VlanStore::new();
        store.replace_device("sw1.example.com".to_string(), vlans(Some(300), &[1, 311]));
        let snapshot = store.device_vlans("sw1.example.com");
        assert_eq!(snapshot.interfaces[&10101].native_vlan, Some(300));
        assert_eq!(snapshot.interfaces[&10101].tagged_vlans, vec![1, 311]);
        assert_eq!(snapshot.names.get(&300).map(String::as_str), Some("Mgmt"));

        store.replace_device("sw1.example.com".to_string(), vlans(Some(1), &[]));
        assert_eq!(store.device_vlans("sw1.example.com").interfaces[&10101].native_vlan, Some(1));
    }

    #[test]
    fn store_unknown_fqdn_is_empty() {
        let snapshot = VlanStore::new().device_vlans("ghost.example.com");
        assert!(snapshot.interfaces.is_empty());
        assert!(snapshot.names.is_empty());
    }

    // --- network_vlans aggregation ---

    fn device(entries: &[(i64, Option<i64>, &[i64])], names: &[(i64, &str)]) -> DeviceVlans {
        DeviceVlans {
            interfaces: entries.iter().map(|(ifindex, native, tagged)| {
                (*ifindex, InterfaceVlans { native_vlan: *native, tagged_vlans: tagged.to_vec() })
            }).collect(),
            names: names.iter().map(|(id, name)| (*id, name.to_string())).collect(),
        }
    }

    #[test]
    fn network_vlans_aggregates_names_and_port_counts() {
        let mut store = VlanStore::new();
        store.replace_device("a.example.com".to_string(), device(
            &[(1, Some(300), &[10, 20]), (2, Some(10), &[])],
            &[(10, "users"), (20, "voice"), (300, "Mgmt")],
        ));
        store.replace_device("b.example.com".to_string(), device(
            &[(1, Some(300), &[10])],
            &[(10, "users"), (300, "management"), (999, "unused")],
        ));

        let vlans = store.network_vlans();
        let ids: Vec<i64> = vlans.iter().map(|v| v.id).collect();
        assert_eq!(ids, vec![10, 20, 300, 999], "sorted by id");

        let v10 = vlans.iter().find(|v| v.id == 10).unwrap();
        assert_eq!(v10.names, vec!["users"], "same name on both devices dedupes");
        assert_eq!(v10.devices.len(), 2);
        assert_eq!(v10.devices[0].fqdn, "a.example.com");
        assert_eq!((v10.devices[0].native_ports, v10.devices[0].tagged_ports), (1, 1));
        assert_eq!((v10.devices[1].native_ports, v10.devices[1].tagged_ports), (0, 1));

        // Conflicting names surface as multiple entries, sorted.
        let v300 = vlans.iter().find(|v| v.id == 300).unwrap();
        assert_eq!(v300.names, vec!["Mgmt", "management"]);

        // A named VLAN with no port usage still appears (0/0).
        let v999 = vlans.iter().find(|v| v.id == 999).unwrap();
        assert_eq!(v999.devices.len(), 1);
        assert_eq!((v999.devices[0].native_ports, v999.devices[0].tagged_ports), (0, 0));
    }

    #[test]
    fn vlan_names_resolves_id_to_distinct_sorted_names() {
        let mut store = VlanStore::new();
        store.replace_device("a.example.com".to_string(), device(&[], &[(10, "users"), (300, "Mgmt")]));
        store.replace_device("b.example.com".to_string(), device(&[], &[(10, "users"), (300, "management")]));

        let names = store.vlan_names();
        // Agreed name dedupes to one entry.
        assert_eq!(names.get(&10).cloned(), Some(vec!["users".to_string()]));
        // Disagreement surfaces as multiple entries, sorted.
        assert_eq!(names.get(&300).cloned(), Some(vec!["Mgmt".to_string(), "management".to_string()]));
        // Unknown id → no entry.
        assert!(names.get(&999).is_none());
    }

    #[test]
    fn network_vlans_includes_unnamed_referenced_vlans() {
        let mut store = VlanStore::new();
        store.replace_device("a.example.com".to_string(), device(&[(1, Some(42), &[])], &[]));
        let vlans = store.network_vlans();
        assert_eq!(vlans.len(), 1);
        assert_eq!(vlans[0].id, 42);
        assert!(vlans[0].names.is_empty());
        assert_eq!(vlans[0].devices[0].name, None);
    }

    #[test]
    fn network_vlans_empty_store_is_empty() {
        assert!(VlanStore::new().network_vlans().is_empty());
    }

    #[test]
    fn store_retain_drops_unmonitored_devices() {
        let mut store = VlanStore::new();
        store.replace_device("keep.example.com".to_string(), vlans(Some(1), &[]));
        store.replace_device("drop.example.com".to_string(), vlans(Some(2), &[]));
        let keep: HashSet<String> = vec!["keep.example.com".to_string()].into_iter().collect();
        store.retain(&keep);
        assert!(!store.device_vlans("keep.example.com").interfaces.is_empty());
        assert!(store.device_vlans("drop.example.com").interfaces.is_empty());
    }
}
