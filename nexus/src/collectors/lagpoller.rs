// In-process link-aggregation (port-channel) collector: which physical ports
// belong to which aggregate, per-member LACP health, and the LACP
// actor/partner system ids used to cross-check both ends of a bundle.
//
// One vendor::Source per incompatible MIB family (probe order and per-device
// winner cache in collectors::vendor):
//   - CiscoLagSource: CISCO-PAGP-MIB::pagpPortTable (pagpGroupIfIndex names
//     the aggregate's ifIndex for every channel mode — on/PAgP/LACP; verified
//     on a live C2960CX), enriched with the IEEE8023-LAG-MIB tables when the
//     bundle runs LACP.
//   - Dot3adLagSource: standard IEEE8023-LAG-MIB only (HP ProCurve "trk"
//     LACP trunks and other standards-based gear).
//
// IF-MIB::ifStackTable is NOT usable for membership on Cisco IOS: verified
// live, it only carries the degenerate 0<->ifIndex rows.
//
// Results live only in the in-memory `LagStore` (no DB, no Prometheus); the
// device detail API joins them onto interfaces and computes the mismatch
// warnings (see `port_channel_warnings`).
extern crate serde_json;

use crate::collectors::entitypoller::{fetch_table, interruptible_sleep, obj_i64, obj_str};
use crate::collectors::poller::{SNMPBotResponse, SNMPBotResultEntryObjectValue};
use crate::collectors::vendor::{self, Vendor};
use crate::db;
use crate::utilities::stp::normalize_mac;
use crate::utilities::tools;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{atomic, Arc, Mutex};

// ---------------------------------------------------------------------------
// Data model: one device's aggregates, keyed by the aggregate's ifIndex.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LagMember {
    // IEEE 802.1AX LacpState bit names as rendered by snmpbot ("lacpActivity",
    // "synchronization", "collecting", "distributing", "defaulted", ...).
    // Empty when the member is configured but not running LACP.
    pub actor_state: Vec<String>,
    pub partner_state: Vec<String>,
    pub partner_system_id: Option<String>, // normalized MAC; None when all-zero
    pub partner_port: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LagGroup {
    // "lacp" (dot3ad rows attached), "pagp" (PAgP negotiation) or "static"
    // (membership known only from pagpGroupIfIndex — channel mode "on", or a
    // configured LACP bundle whose members are all down).
    pub protocol: String,
    pub actor_system_id: Option<String>,   // this device's LACP system id
    pub partner_system_id: Option<String>, // the far end's, per the agg table
    pub members: BTreeMap<i64, LagMember>, // keyed by member ifIndex
}

#[derive(Clone, Default)]
pub struct DeviceLags {
    pub groups: BTreeMap<i64, LagGroup>, // keyed by aggregate ifIndex
}

pub struct LagStore {
    devices: HashMap<String, DeviceLags>, // keyed by fqdn
}

impl LagStore {
    pub fn new() -> LagStore {
        LagStore { devices: HashMap::new() }
    }

    fn replace_device(&mut self, fqdn: String, lags: DeviceLags) {
        self.devices.insert(fqdn, lags);
    }

    fn retain(&mut self, keep: &HashSet<String>) {
        self.devices.retain(|fqdn, _| keep.contains(fqdn));
    }

    // None = never successfully polled (vs Some with zero groups: polled, no
    // aggregates). The distinction gates the far-end cross-check.
    pub fn device_lags(&self, fqdn: &str) -> Option<DeviceLags> {
        self.devices.get(fqdn).cloned()
    }
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

// BITS values arrive as a JSON array of bit names.
fn obj_bits(objects: &HashMap<String, SNMPBotResultEntryObjectValue>, key: &str) -> Vec<String> {
    match objects.get(key) {
        Some(SNMPBotResultEntryObjectValue::Other(serde_json::Value::Array(items))) => {
            items.iter().filter_map(|v| v.as_str().map(String::from)).collect()
        }
        _ => Vec::new(),
    }
}

// All-zero partner/actor system ids mean "nobody".
fn nonzero_mac(mac: Option<String>) -> Option<String> {
    let mac = normalize_mac(&mac?);
    if mac.is_empty() || mac == "00:00:00:00:00:00" { None } else { Some(mac) }
}

// pagpGroupIfIndex semantics (verified live): bundled ports report the
// aggregate's ifIndex; unbundled ports report 0 or their own ifIndex.
pub(crate) fn decode_pagp(pagp: &SNMPBotResponse) -> (BTreeMap<i64, BTreeSet<i64>>, HashMap<i64, i64>) {
    let mut groups: BTreeMap<i64, BTreeSet<i64>> = BTreeMap::new();
    let mut modes: HashMap<i64, i64> = HashMap::new();
    for entry in pagp.entries.iter() {
        let ifindex = match entry.index.get("IF-MIB::ifIndex") {
            Some(v) => *v,
            None => continue,
        };
        if let Some(mode) = obj_i64(&entry.objects, "CISCO-PAGP-MIB::pagpEthcOperationMode") {
            modes.insert(ifindex, mode);
        }
        match obj_i64(&entry.objects, "CISCO-PAGP-MIB::pagpGroupIfIndex") {
            Some(group) if group > 0 && group != ifindex => {
                groups.entry(group).or_insert_with(BTreeSet::new).insert(ifindex);
            }
            _ => {}
        }
    }
    (groups, modes)
}

// IEEE8023-LAG-MIB: membership from dot3adAggPortAttachedAggID (0 = detached,
// rows are all-zero on gear where LACP is idle), per-aggregate system ids from
// dot3adAggTable. Only aggregates with at least one attached member appear.
pub(crate) fn decode_dot3ad(
    agg_table: Option<&SNMPBotResponse>,
    port_table: Option<&SNMPBotResponse>,
) -> BTreeMap<i64, LagGroup> {
    let mut groups: BTreeMap<i64, LagGroup> = BTreeMap::new();

    for entry in port_table.map(|t| t.entries.iter()).into_iter().flatten() {
        let ifindex = match entry.index.get("IEEE8023-LAG-MIB::dot3adAggPortIndex") {
            Some(v) => *v,
            None => continue,
        };
        let attached = obj_i64(&entry.objects, "IEEE8023-LAG-MIB::dot3adAggPortAttachedAggID").unwrap_or(0);
        // A port attached to itself is an "individual" link, not a bundle.
        if attached <= 0 || attached == ifindex {
            continue;
        }
        let member = LagMember {
            actor_state: obj_bits(&entry.objects, "IEEE8023-LAG-MIB::dot3adAggPortActorOperState"),
            partner_state: obj_bits(&entry.objects, "IEEE8023-LAG-MIB::dot3adAggPortPartnerOperState"),
            partner_system_id: nonzero_mac(obj_str(&entry.objects, "IEEE8023-LAG-MIB::dot3adAggPortPartnerOperSystemID")),
            partner_port: obj_i64(&entry.objects, "IEEE8023-LAG-MIB::dot3adAggPortPartnerOperPort").filter(|p| *p > 0),
        };
        groups
            .entry(attached)
            .or_insert_with(|| LagGroup {
                protocol: "lacp".to_string(),
                actor_system_id: None,
                partner_system_id: None,
                members: BTreeMap::new(),
            })
            .members
            .insert(ifindex, member);
    }

    for entry in agg_table.map(|t| t.entries.iter()).into_iter().flatten() {
        let agg = match entry.index.get("IEEE8023-LAG-MIB::dot3adAggIndex") {
            Some(v) => *v,
            None => continue,
        };
        if let Some(group) = groups.get_mut(&agg) {
            group.actor_system_id = nonzero_mac(obj_str(&entry.objects, "IEEE8023-LAG-MIB::dot3adAggActorSystemID"));
            group.partner_system_id = nonzero_mac(obj_str(&entry.objects, "IEEE8023-LAG-MIB::dot3adAggPartnerSystemID"));
        }
    }

    groups
}

// Cisco merge: dot3ad carries the LACP detail, pagpGroupIfIndex is the
// authoritative membership (it also lists configured members that LACP has
// not attached — down links, mode-on channels). PagpEthcOperationMode: 1=off
// (LACP or mode on), higher values = PAgP negotiation.
pub(crate) fn merge_cisco(
    dot3ad: BTreeMap<i64, LagGroup>,
    pagp_groups: BTreeMap<i64, BTreeSet<i64>>,
    pagp_modes: &HashMap<i64, i64>,
) -> DeviceLags {
    let mut groups = dot3ad;
    for (agg, members) in pagp_groups.into_iter() {
        let group = groups.entry(agg).or_insert_with(|| LagGroup {
            protocol: if members.iter().any(|m| pagp_modes.get(m).map(|mode| *mode > 1).unwrap_or(false)) {
                "pagp".to_string()
            } else {
                "static".to_string()
            },
            actor_system_id: None,
            partner_system_id: None,
            members: BTreeMap::new(),
        });
        for member in members.into_iter() {
            group.members.entry(member).or_insert_with(LagMember::default);
        }
    }
    DeviceLags { groups }
}

// ---------------------------------------------------------------------------
// Mismatch warnings, computed per aggregate for the device detail API.
// ---------------------------------------------------------------------------

pub struct MemberMeta {
    pub name: String,                       // interface name for the warning label
    pub connected_to_fqdn: Option<String>,  // discovered link peer, if any
}

pub fn lacp_bundled(state: &[String]) -> bool {
    ["synchronization", "collecting", "distributing"].iter().all(|bit| state.iter().any(|s| s == bit))
}

// `meta` maps member ifIndex -> interface metadata; `peer_lags` holds the LAG
// data of monitored peer devices (only entries that have actually been
// polled). Warnings are stable, machine-matchable codes with the member
// interface name suffixed where relevant.
//
// Deliberately absent: comparing the LACP partner system id against the
// peer's dot1dBase bridge MAC — a switch's LACP system MAC is not guaranteed
// to equal its bridge MAC, so that check would false-positive. The far-end
// cross-check below compares LACP system ids on both sides instead.
pub fn port_channel_warnings(
    group: &LagGroup,
    meta: &HashMap<i64, MemberMeta>,
    peer_lags: &HashMap<String, DeviceLags>,
) -> Vec<String> {
    let mut warnings: Vec<String> = Vec::new();
    let label = |ifindex: &i64| -> String {
        meta.get(ifindex).map(|m| m.name.clone()).unwrap_or_else(|| ifindex.to_string())
    };

    if group.protocol != "lacp" {
        warnings.push(format!("not-lacp:{}", group.protocol));
    }
    if group.members.len() < 2 {
        warnings.push("single-member".to_string());
    }

    for (ifindex, member) in group.members.iter() {
        // defaulted/expired: the port is sending LACPDUs but hearing nothing
        // back — the far end is not running LACP on that link.
        if member.actor_state.iter().any(|s| s == "defaulted" || s == "expired") {
            warnings.push(format!("member-no-lacp-partner:{}", label(ifindex)));
        } else if group.protocol == "lacp" && !lacp_bundled(&member.actor_state) {
            warnings.push(format!("member-not-bundled:{}", label(ifindex)));
        }
    }

    // All members must agree on who the partner is (LACP view)...
    let partner_ids: BTreeSet<&String> = group.members.values().filter_map(|m| m.partner_system_id.as_ref()).collect();
    if partner_ids.len() > 1 {
        warnings.push("members-report-different-partners".to_string());
    }
    // ...and the discovered topology must agree on where the cables go.
    let peer_fqdns: BTreeSet<&String> = group
        .members
        .keys()
        .filter_map(|ifindex| meta.get(ifindex).and_then(|m| m.connected_to_fqdn.as_ref()))
        .collect();
    if peer_fqdns.len() > 1 {
        warnings.push("members-wired-to-different-devices".to_string());
    }

    // Far-end cross-check: when the (single) peer device is monitored and has
    // LAG data, it must have an aggregate whose partner is us, with the same
    // member count. Matched by LACP system id, so it needs the LACP detail on
    // both sides.
    if peer_fqdns.len() == 1 {
        if let Some(peer) = peer_lags.get(*peer_fqdns.iter().next().unwrap()) {
            if let Some(actor_id) = group.actor_system_id.as_ref() {
                match peer.groups.values().find(|g| g.partner_system_id.as_ref() == Some(actor_id)) {
                    None => warnings.push("far-end-lag-not-found".to_string()),
                    Some(peer_group) => {
                        if peer_group.members.len() != group.members.len() {
                            warnings.push("far-end-member-count-mismatch".to_string());
                        }
                    }
                }
            }
        }
    }

    warnings
}

// ---------------------------------------------------------------------------
// Vendor sources
// ---------------------------------------------------------------------------

struct LagCtx<'a> {
    snmpbot_url: &'a String,
    host: &'a String,
}

struct CiscoLagSource;

impl<'a> vendor::Source<LagCtx<'a>> for CiscoLagSource {
    type Output = DeviceLags;

    fn name(&self) -> &'static str {
        "cisco-pagp"
    }

    fn vendor(&self) -> Vendor {
        Vendor::Cisco
    }

    fn collect(&self, ctx: &LagCtx) -> Option<DeviceLags> {
        // pagpPortTable answers (with rows) on Cisco switches even when no
        // channel is configured, so it doubles as the applicability probe.
        let pagp = match fetch_table(ctx.snmpbot_url, ctx.host, "CISCO-PAGP-MIB::pagpPortTable") {
            Some(t) if !t.entries.is_empty() => t,
            _ => return None,
        };
        let (pagp_groups, pagp_modes) = decode_pagp(&pagp);
        if pagp_groups.is_empty() {
            // No aggregates: skip the dot3ad walks entirely.
            return Some(DeviceLags::default());
        }
        let agg = fetch_table(ctx.snmpbot_url, ctx.host, "IEEE8023-LAG-MIB::dot3adAggTable");
        let ports = fetch_table(ctx.snmpbot_url, ctx.host, "IEEE8023-LAG-MIB::dot3adAggPortTable");
        Some(merge_cisco(decode_dot3ad(agg.as_ref(), ports.as_ref()), pagp_groups, &pagp_modes))
    }
}

struct Dot3adLagSource;

impl<'a> vendor::Source<LagCtx<'a>> for Dot3adLagSource {
    type Output = DeviceLags;

    fn name(&self) -> &'static str {
        "dot3ad"
    }

    fn vendor(&self) -> Vendor {
        Vendor::Generic
    }

    fn collect(&self, ctx: &LagCtx) -> Option<DeviceLags> {
        // An empty walk cannot distinguish "no aggregates" from "MIB not
        // supported", so a device without LACP trunks re-probes each cycle.
        let ports = fetch_table(ctx.snmpbot_url, ctx.host, "IEEE8023-LAG-MIB::dot3adAggPortTable")?;
        let agg = fetch_table(ctx.snmpbot_url, ctx.host, "IEEE8023-LAG-MIB::dot3adAggTable");
        let groups = decode_dot3ad(agg.as_ref(), Some(&ports));
        if groups.is_empty() {
            return None;
        }
        Some(DeviceLags { groups })
    }
}

fn poll_device(snmpbot_url: &String, fqdn: &String, community: &String, hint: Vendor, sources_cache: &vendor::SourceCache) -> Option<DeviceLags> {
    let host = format!("{}@{}", community, fqdn);
    let ctx = LagCtx { snmpbot_url: snmpbot_url, host: &host };
    let sources: [&dyn vendor::Source<LagCtx, Output = DeviceLags>; 2] = [&CiscoLagSource, &Dot3adLagSource];
    vendor::collect_first(sources_cache, fqdn, hint, &sources, &ctx)
}

// ---------------------------------------------------------------------------
// Supervisor
// ---------------------------------------------------------------------------

struct LagDevice {
    fqdn: String,
    community: String,
    vendor: Vendor,
}

fn load_devices(pool: &db::Pool) -> Vec<LagDevice> {
    let mut devices: Vec<LagDevice> = Vec::new();
    if let Ok(mut conn) = pool.get() {
        for device in crate::models::dbo::Device::monitored(&mut *conn).iter() {
            let community = match device.snmp_community {
                Some(ref c) => c.clone(),
                None => continue,
            };
            devices.push(LagDevice {
                fqdn: format!("{}.{}", device.name, device.dns_domain),
                community: community,
                vendor: vendor::vendor_hint(device.os_info.as_deref(), device.device_type.as_deref()),
            });
        }
    } else {
        println!("[lagpoller] failed to acquire db connection for device listing");
    }
    devices
}

const MAX_POLL_WORKERS: usize = 16;

pub fn run(
    snmpbot_url: String,
    interval_msecs: u64,
    store: Arc<Mutex<LagStore>>,
    running: Arc<atomic::AtomicBool>,
) {
    println!("[lagpoller] starting in-process collector (snmpbot={}, interval_msecs={})", snmpbot_url, interval_msecs);
    let pool = db::connect();
    let no_jitter = std::env::var("JASPY_POLLER_NO_JITTER").map(|v| v == "1" || v == "true").unwrap_or(false);
    let sources_cache = Arc::new(vendor::SourceCache::new());

    while running.load(atomic::Ordering::Relaxed) {
        let cycle_start = tools::get_time_msecs();
        let devices = load_devices(&pool);
        let keep: HashSet<String> = devices.iter().map(|d| d.fqdn.clone()).collect();
        if let Ok(mut store) = store.lock() {
            store.retain(&keep);
        }
        sources_cache.retain(&keep);

        let jitter = if no_jitter { 0 } else { interval_msecs / 2 };
        crate::collectors::pool::run_bounded(devices, MAX_POLL_WORKERS, jitter, |device| {
            // Only replace on success so a transient failure keeps the
            // previous data.
            if let Some(lags) = poll_device(&snmpbot_url, &device.fqdn, &device.community, device.vendor, &sources_cache) {
                if let Ok(mut store) = store.lock() {
                    store.replace_device(device.fqdn, lags);
                }
            }
        });

        let elapsed = tools::get_time_msecs() - cycle_start;
        if elapsed < interval_msecs {
            interruptible_sleep(interval_msecs - elapsed, &running);
        }
    }
    println!("[lagpoller] collector stopped");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const PAGP: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pagpporttable.json"));
    const AGG: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dot3adaggtable.json"));
    const AGG_PORTS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dot3adaggporttable.json"));

    fn parse(fixture: &str) -> SNMPBotResponse {
        serde_json::from_str(fixture).unwrap()
    }

    // --- decode_pagp ---

    #[test]
    fn pagp_groups_bundled_ports_and_skips_self_and_zero() {
        let (groups, modes) = decode_pagp(&parse(PAGP));
        assert_eq!(groups.len(), 1);
        let members: Vec<i64> = groups[&5001].iter().cloned().collect();
        assert_eq!(members, vec![10101, 10102]);
        // Unbundled forms: group 0 (10103) and group == self (10104).
        assert_eq!(modes[&10103], 1);
        assert_eq!(modes[&10104], 1);
    }

    // --- decode_dot3ad ---

    #[test]
    fn dot3ad_decodes_attached_members_with_state_and_partner() {
        let groups = decode_dot3ad(Some(&parse(AGG)), Some(&parse(AGG_PORTS)));
        assert_eq!(groups.len(), 1);
        let group = &groups[&5001];
        assert_eq!(group.protocol, "lacp");
        assert_eq!(group.actor_system_id.as_deref(), Some("aa:bb:cc:dd:ee:01"));
        assert_eq!(group.partner_system_id.as_deref(), Some("aa:bb:cc:dd:ee:02"));
        assert_eq!(group.members.len(), 2, "the all-zero row (10103) is not a member");
        let member = &group.members[&10101];
        assert!(member.actor_state.iter().any(|s| s == "collecting"));
        assert!(member.partner_state.iter().any(|s| s == "lacpTimeout"));
        assert_eq!(member.partner_system_id.as_deref(), Some("aa:bb:cc:dd:ee:02"));
        assert_eq!(member.partner_port, Some(5));
    }

    #[test]
    fn dot3ad_without_agg_table_still_yields_members() {
        let groups = decode_dot3ad(None, Some(&parse(AGG_PORTS)));
        let group = &groups[&5001];
        assert_eq!(group.actor_system_id, None);
        assert_eq!(group.members.len(), 2);
    }

    #[test]
    fn dot3ad_all_idle_is_empty() {
        // Live shape of a Cisco switch with LACP idle: rows exist, all zero.
        let idle = r#"{
            "ID": "IEEE8023-LAG-MIB::dot3adAggPortTable",
            "IndexKeys": ["IEEE8023-LAG-MIB::dot3adAggPortIndex"],
            "ObjectKeys": ["IEEE8023-LAG-MIB::dot3adAggPortAttachedAggID"],
            "Entries": [{
                "HostID": "h",
                "Index": {"IEEE8023-LAG-MIB::dot3adAggPortIndex": 10101},
                "Objects": {
                    "IEEE8023-LAG-MIB::dot3adAggPortAttachedAggID": 0,
                    "IEEE8023-LAG-MIB::dot3adAggPortPartnerOperSystemID": "00:00:00:00:00:00",
                    "IEEE8023-LAG-MIB::dot3adAggPortActorOperState": []
                }
            }]
        }"#;
        assert!(decode_dot3ad(None, Some(&parse(idle))).is_empty());
    }

    // --- merge_cisco ---

    #[test]
    fn cisco_merge_enriches_pagp_membership_with_lacp_detail() {
        let (pagp_groups, modes) = decode_pagp(&parse(PAGP));
        let lags = merge_cisco(decode_dot3ad(Some(&parse(AGG)), Some(&parse(AGG_PORTS))), pagp_groups, &modes);
        let group = &lags.groups[&5001];
        assert_eq!(group.protocol, "lacp");
        assert_eq!(group.members.len(), 2);
        assert!(lacp_bundled(&group.members[&10101].actor_state));
    }

    #[test]
    fn cisco_merge_pagp_only_membership_is_static() {
        let (pagp_groups, modes) = decode_pagp(&parse(PAGP));
        // No dot3ad data at all: mode-on channel (or all members down).
        let lags = merge_cisco(BTreeMap::new(), pagp_groups, &modes);
        let group = &lags.groups[&5001];
        assert_eq!(group.protocol, "static");
        assert_eq!(group.members.len(), 2);
        assert!(group.members[&10101].actor_state.is_empty());
    }

    #[test]
    fn cisco_merge_adds_configured_but_detached_members() {
        // dot3ad attached one member; pagp knows both (second link down).
        let (pagp_groups, modes) = decode_pagp(&parse(PAGP));
        let mut dot3ad = decode_dot3ad(Some(&parse(AGG)), Some(&parse(AGG_PORTS)));
        dot3ad.get_mut(&5001).unwrap().members.remove(&10102);
        let lags = merge_cisco(dot3ad, pagp_groups, &modes);
        let group = &lags.groups[&5001];
        assert_eq!(group.protocol, "lacp");
        assert_eq!(group.members.len(), 2);
        assert!(group.members[&10102].actor_state.is_empty(), "detached member has no LACP state");
    }

    #[test]
    fn cisco_merge_pagp_mode_marks_pagp_protocol() {
        let mut modes = HashMap::new();
        modes.insert(10101, 3i64); // desirable
        modes.insert(10102, 3i64);
        let mut pagp_groups = BTreeMap::new();
        pagp_groups.insert(5001, vec![10101i64, 10102].into_iter().collect());
        let lags = merge_cisco(BTreeMap::new(), pagp_groups, &modes);
        assert_eq!(lags.groups[&5001].protocol, "pagp");
    }

    // --- warnings ---

    fn healthy_group() -> LagGroup {
        let mut lags = merge_cisco(
            decode_dot3ad(Some(&parse(AGG)), Some(&parse(AGG_PORTS))),
            decode_pagp(&parse(PAGP)).0,
            &decode_pagp(&parse(PAGP)).1,
        );
        lags.groups.remove(&5001).unwrap()
    }

    fn meta(entries: &[(i64, &str, Option<&str>)]) -> HashMap<i64, MemberMeta> {
        entries
            .iter()
            .map(|(ifindex, name, peer)| {
                (*ifindex, MemberMeta { name: name.to_string(), connected_to_fqdn: peer.map(String::from) })
            })
            .collect()
    }

    #[test]
    fn healthy_two_member_lacp_group_has_no_warnings() {
        let meta = meta(&[(10101, "Gi0/1", Some("sw2.x")), (10102, "Gi0/2", Some("sw2.x"))]);
        assert!(port_channel_warnings(&healthy_group(), &meta, &HashMap::new()).is_empty());
    }

    #[test]
    fn defaulted_member_warns_no_lacp_partner() {
        let mut group = healthy_group();
        group.members.get_mut(&10102).unwrap().actor_state = vec!["lacpActivity".to_string(), "defaulted".to_string()];
        group.members.get_mut(&10102).unwrap().partner_system_id = None;
        let meta = meta(&[(10101, "Gi0/1", None), (10102, "Gi0/2", None)]);
        let warnings = port_channel_warnings(&group, &meta, &HashMap::new());
        assert_eq!(warnings, vec!["member-no-lacp-partner:Gi0/2"]);
    }

    #[test]
    fn detached_member_warns_not_bundled() {
        let mut group = healthy_group();
        group.members.get_mut(&10102).unwrap().actor_state = Vec::new();
        group.members.get_mut(&10102).unwrap().partner_system_id = None;
        let warnings = port_channel_warnings(&group, &meta(&[(10101, "Gi0/1", None), (10102, "Gi0/2", None)]), &HashMap::new());
        assert_eq!(warnings, vec!["member-not-bundled:Gi0/2"]);
    }

    #[test]
    fn non_lacp_and_single_member_warn() {
        let mut group = healthy_group();
        group.protocol = "static".to_string();
        group.members.remove(&10102);
        let warnings = port_channel_warnings(&group, &meta(&[(10101, "Gi0/1", None)]), &HashMap::new());
        assert_eq!(warnings, vec!["not-lacp:static", "single-member"]);
    }

    #[test]
    fn members_reporting_different_partners_warn() {
        let mut group = healthy_group();
        group.members.get_mut(&10102).unwrap().partner_system_id = Some("aa:bb:cc:dd:ee:99".to_string());
        let warnings = port_channel_warnings(&group, &meta(&[(10101, "Gi0/1", None), (10102, "Gi0/2", None)]), &HashMap::new());
        assert_eq!(warnings, vec!["members-report-different-partners"]);
    }

    #[test]
    fn members_wired_to_different_devices_warn() {
        let group = healthy_group();
        let meta = meta(&[(10101, "Gi0/1", Some("sw2.x")), (10102, "Gi0/2", Some("sw3.x"))]);
        let warnings = port_channel_warnings(&group, &meta, &HashMap::new());
        assert_eq!(warnings, vec!["members-wired-to-different-devices"]);
    }

    #[test]
    fn far_end_cross_check_matches_by_system_ids() {
        let group = healthy_group();
        let meta = meta(&[(10101, "Gi0/1", Some("sw2.x")), (10102, "Gi0/2", Some("sw2.x"))]);

        // Peer has a mirrored group: partner == our actor id, same size.
        let mut peer_group = healthy_group();
        peer_group.actor_system_id = Some("aa:bb:cc:dd:ee:02".to_string());
        peer_group.partner_system_id = Some("aa:bb:cc:dd:ee:01".to_string());
        let mut peer = DeviceLags::default();
        peer.groups.insert(1, peer_group);
        let mut peers = HashMap::new();
        peers.insert("sw2.x".to_string(), peer);
        assert!(port_channel_warnings(&group, &meta, &peers).is_empty());

        // Peer polled but has no group pointing back at us.
        let mut peers = HashMap::new();
        peers.insert("sw2.x".to_string(), DeviceLags::default());
        assert_eq!(port_channel_warnings(&group, &meta, &peers), vec!["far-end-lag-not-found"]);

        // Peer's mirrored group has a different member count.
        let mut peer_group = healthy_group();
        peer_group.partner_system_id = Some("aa:bb:cc:dd:ee:01".to_string());
        peer_group.members.remove(&10102);
        let mut peer = DeviceLags::default();
        peer.groups.insert(1, peer_group);
        let mut peers = HashMap::new();
        peers.insert("sw2.x".to_string(), peer);
        assert_eq!(port_channel_warnings(&group, &meta, &peers), vec!["far-end-member-count-mismatch"]);

        // Unmonitored peer: no cross-check, no warning.
        assert!(port_channel_warnings(&group, &meta, &HashMap::new()).is_empty());
    }

    // --- store ---

    #[test]
    fn store_distinguishes_unpolled_from_empty() {
        let mut store = LagStore::new();
        assert!(store.device_lags("sw1.x").is_none());
        store.replace_device("sw1.x".to_string(), DeviceLags::default());
        let lags = store.device_lags("sw1.x").unwrap();
        assert!(lags.groups.is_empty());
    }

    #[test]
    fn store_retain_drops_unmonitored_devices() {
        let mut store = LagStore::new();
        store.replace_device("keep.x".to_string(), DeviceLags::default());
        store.replace_device("drop.x".to_string(), DeviceLags::default());
        let keep: HashSet<String> = vec!["keep.x".to_string()].into_iter().collect();
        store.retain(&keep);
        assert!(store.device_lags("keep.x").is_some());
        assert!(store.device_lags("drop.x").is_none());
    }
}
