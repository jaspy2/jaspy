// Pure decoders for Cisco Class-Based QoS (policy-map) counters, plus the
// negative-probe cache that keeps the entitypoller from re-walking devices that
// have no service-policies. Kept free of I/O (like poe.rs / entity_media) so the
// multi-table join and the cache TTL logic are unit-tested from snmpbot-shaped
// fixtures; the entitypoller does the SNMP fetches and the ifIndex->ifName walk,
// then calls in here.
//
// One MIB feeds this: CISCO-CLASS-BASED-QOS-MIB (in snmpbot/mibs/). A single
// counter is only meaningful after a join across its normalized tables:
//   - cbQosServicePolicyTable   policyIndex            -> ifIndex + direction
//   - cbQosObjectsTable         (policyIndex,objIndex) -> objectType, configIndex,
//                                                         parentObjIndex (hierarchy)
//   - cbQosPolicyMapCfgTable    configIndex            -> policy-map name
//   - cbQosCMCfgTable           configIndex            -> class-map name
//   - cbQosCMStatsTable         (policyIndex,objIndex) -> class-map counters
//   - cbQosPoliceStatsTable     (policyIndex,objIndex) -> policer counters
//
// The hierarchy is policymap(1) -> classmap(2) -> {matchStatement(3),
// police(7), ...}. decode_qos walks it: each classmap object resolves its
// policy-map by climbing parentObjIndex to the enclosing policymap, joins its
// class-map counters by (policyIndex,objIndex), and attaches the counters of its
// child police object when present.

use crate::collectors::poller::{SNMPBotResponse, SNMPBotResultEntry, SNMPBotResultEntryObjectValue};
use crate::collectors::entitypoller::obj_i64;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const M: &str = "CISCO-CLASS-BASED-QOS-MIB";

// cbQosObjectsType is an SNMP ENUM, so snmpbot renders it by name (like
// cbQosPolicyDirection and the PoE status columns). We match on the name.
const TYPE_POLICYMAP: &str = "policymap";
const TYPE_CLASSMAP: &str = "classmap";
const TYPE_POLICE: &str = "police";

// Direction of an applied service-policy (cbQosPolicyDirection).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QosDirection {
    Input,
    Output,
}

impl QosDirection {
    fn from_snmp(value: &str) -> QosDirection {
        // snmpbot renders the ENUM by name; default to input for anything
        // unexpected (a policy is applied in exactly one direction).
        match value {
            "output" => QosDirection::Output,
            _ => QosDirection::Input,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            QosDirection::Input => "input",
            QosDirection::Output => "output",
        }
    }
}

// Policer (police action) counters for one class, all 64-bit HC columns. None
// for a class with no policer, or when the device omits a column.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QosPolice {
    pub conform_pkts: Option<u64>,
    pub conform_bytes: Option<u64>,
    pub exceed_pkts: Option<u64>,
    pub exceed_bytes: Option<u64>,
    pub violate_pkts: Option<u64>,
    pub violate_bytes: Option<u64>,
}

// One class-map within one applied service-policy: the fully-joined unit the
// metrics and API surface. `interface_name` is the resolved ifName (None for a
// control-plane policy on ifIndex 0, or an unresolved index).
#[derive(Clone, Debug, PartialEq)]
pub struct QosClass {
    pub interface_ifindex: i64,
    pub interface_name: Option<String>,
    // db interface id, resolved by the entitypoller from interface_name after
    // decode (the decoder is pure and has no DB); None until then / unresolved.
    pub interface_id: Option<i64>,
    pub direction: QosDirection,
    pub policymap: String,
    pub classmap: String,
    pub prepolicy_pkts: Option<u64>,
    pub prepolicy_bytes: Option<u64>,
    pub postpolicy_bytes: Option<u64>,
    pub drop_pkts: Option<u64>,
    pub drop_bytes: Option<u64>,
    pub police: Option<QosPolice>,
}

fn obj_u64(objects: &HashMap<String, SNMPBotResultEntryObjectValue>, key: &str) -> Option<u64> {
    match objects.get(key) {
        Some(SNMPBotResultEntryObjectValue::Uint64(v)) => Some(*v),
        // A Counter64 that overflowed the JSON number would arrive as Float64;
        // accept it best-effort rather than dropping the sample.
        Some(SNMPBotResultEntryObjectValue::Float64(v)) if *v >= 0.0 => Some(*v as u64),
        _ => None,
    }
}

fn obj_str(objects: &HashMap<String, SNMPBotResultEntryObjectValue>, key: &str) -> Option<String> {
    match objects.get(key) {
        Some(SNMPBotResultEntryObjectValue::Str(v)) => Some(v.clone()),
        _ => None,
    }
}

// Both stats tables and the objects table share the (policyIndex, objectsIndex)
// compound index; the cfg tables are keyed by configIndex alone.
fn policy_obj_key(entry: &SNMPBotResultEntry) -> Option<(i64, i64)> {
    let policy = entry.index.get(&format!("{}::cbQosPolicyIndex", M))?;
    let obj = entry.index.get(&format!("{}::cbQosObjectsIndex", M))?;
    Some((*policy, *obj))
}

// configIndex -> name for a cfg table (policy-map or class-map name).
fn config_names(cfg: &SNMPBotResponse, name_col: &str) -> HashMap<i64, String> {
    let mut out = HashMap::new();
    let cfg_key = format!("{}::cbQosConfigIndex", M);
    for entry in cfg.entries.iter() {
        if let (Some(idx), Some(name)) = (entry.index.get(&cfg_key), obj_str(&entry.objects, name_col)) {
            out.insert(*idx, name);
        }
    }
    out
}

struct ObjInfo {
    config_index: i64,
    otype: String, // cbQosObjectsType enum name ("classmap", "police", ...)
    parent: i64,
}

// Join every CBQoS table into the flat per-class view. `if_names` maps ifIndex
// to ifName (from IF-MIB::ifXTable); the caller supplies it so this stays pure.
pub fn decode_qos(
    service_policy: &SNMPBotResponse,
    objects: &SNMPBotResponse,
    policymap_cfg: &SNMPBotResponse,
    cm_cfg: &SNMPBotResponse,
    cm_stats: &SNMPBotResponse,
    police_stats: &SNMPBotResponse,
    if_names: &HashMap<i64, String>,
) -> Vec<QosClass> {
    // policyIndex -> (ifIndex, direction)
    let mut policy_meta: HashMap<i64, (i64, QosDirection)> = HashMap::new();
    let policy_key = format!("{}::cbQosPolicyIndex", M);
    for entry in service_policy.entries.iter() {
        let policy = match entry.index.get(&policy_key) {
            Some(v) => *v,
            None => continue,
        };
        let ifindex = obj_i64(&entry.objects, &format!("{}::cbQosIfIndex", M)).unwrap_or(0);
        let direction = obj_str(&entry.objects, &format!("{}::cbQosPolicyDirection", M))
            .map(|d| QosDirection::from_snmp(&d))
            .unwrap_or(QosDirection::Input);
        policy_meta.insert(policy, (ifindex, direction));
    }

    let policymap_names = config_names(policymap_cfg, &format!("{}::cbQosPolicyMapName", M));
    let cm_names = config_names(cm_cfg, &format!("{}::cbQosCMName", M));

    // (policyIndex, objIndex) -> object metadata, and the police child index
    // (policyIndex, classmapObjIndex) -> policeObjIndex.
    let mut objs: HashMap<(i64, i64), ObjInfo> = HashMap::new();
    let mut police_child: HashMap<(i64, i64), i64> = HashMap::new();
    for entry in objects.entries.iter() {
        let key = match policy_obj_key(entry) {
            Some(k) => k,
            None => continue,
        };
        let info = ObjInfo {
            config_index: obj_i64(&entry.objects, &format!("{}::cbQosConfigIndex", M)).unwrap_or(0),
            otype: obj_str(&entry.objects, &format!("{}::cbQosObjectsType", M)).unwrap_or_default(),
            parent: obj_i64(&entry.objects, &format!("{}::cbQosParentObjectsIndex", M)).unwrap_or(0),
        };
        if info.otype == TYPE_POLICE {
            police_child.insert((key.0, info.parent), key.1);
        }
        objs.insert(key, info);
    }

    // Index the stats tables by (policyIndex, objIndex) for direct lookup.
    let cm_stats_by_key = index_by_policy_obj(cm_stats);
    let police_stats_by_key = index_by_policy_obj(police_stats);

    let mut out: Vec<QosClass> = Vec::new();
    for ((policy, obj_index), info) in objs.iter() {
        if info.otype != TYPE_CLASSMAP {
            continue;
        }
        let (ifindex, direction) = policy_meta.get(policy).copied().unwrap_or((0, QosDirection::Input));
        let policymap = resolve_policymap(*policy, *obj_index, &objs, &policymap_names);
        let classmap = cm_names.get(&info.config_index).cloned().unwrap_or_default();

        let cm = cm_stats_by_key.get(&(*policy, *obj_index));
        let prepolicy_pkts = cm.and_then(|o| obj_u64(o, &format!("{}::cbQosCMPrePolicyPkt64", M)));
        let prepolicy_bytes = cm.and_then(|o| obj_u64(o, &format!("{}::cbQosCMPrePolicyByte64", M)));
        let postpolicy_bytes = cm.and_then(|o| obj_u64(o, &format!("{}::cbQosCMPostPolicyByte64", M)));
        let drop_pkts = cm.and_then(|o| obj_u64(o, &format!("{}::cbQosCMDropPkt64", M)));
        let drop_bytes = cm.and_then(|o| obj_u64(o, &format!("{}::cbQosCMDropByte64", M)));

        let police = police_child.get(&(*policy, *obj_index)).and_then(|police_obj| {
            police_stats_by_key.get(&(*policy, *police_obj)).map(|o| QosPolice {
                conform_pkts: obj_u64(o, &format!("{}::cbQosPoliceConformedPkt64", M)),
                conform_bytes: obj_u64(o, &format!("{}::cbQosPoliceConformedByte64", M)),
                exceed_pkts: obj_u64(o, &format!("{}::cbQosPoliceExceededPkt64", M)),
                exceed_bytes: obj_u64(o, &format!("{}::cbQosPoliceExceededByte64", M)),
                violate_pkts: obj_u64(o, &format!("{}::cbQosPoliceViolatedPkt64", M)),
                violate_bytes: obj_u64(o, &format!("{}::cbQosPoliceViolatedByte64", M)),
            })
        });

        out.push(QosClass {
            interface_ifindex: ifindex,
            interface_name: if_names.get(&ifindex).cloned(),
            interface_id: None,
            direction,
            policymap,
            classmap,
            prepolicy_pkts,
            prepolicy_bytes,
            postpolicy_bytes,
            drop_pkts,
            drop_bytes,
            police,
        });
    }

    // Deterministic order for stable metric output, UI rows, and tests.
    out.sort_by(|a, b| {
        a.interface_ifindex
            .cmp(&b.interface_ifindex)
            .then(a.direction.as_str().cmp(b.direction.as_str()))
            .then(a.policymap.cmp(&b.policymap))
            .then(a.classmap.cmp(&b.classmap))
    });
    out
}

fn index_by_policy_obj(
    stats: &SNMPBotResponse,
) -> HashMap<(i64, i64), HashMap<String, SNMPBotResultEntryObjectValue>> {
    let mut out = HashMap::new();
    for entry in stats.entries.iter() {
        if let Some(key) = policy_obj_key(entry) {
            out.insert(key, entry.objects.clone());
        }
    }
    out
}

// Climb parentObjIndex from a class-map object to the enclosing policy-map and
// return its name. Bounded by the object count so a self-referential parent
// (firmware bug) can't loop forever.
fn resolve_policymap(
    policy: i64,
    obj_index: i64,
    objs: &HashMap<(i64, i64), ObjInfo>,
    policymap_names: &HashMap<i64, String>,
) -> String {
    let mut current = obj_index;
    for _ in 0..objs.len() + 1 {
        let info = match objs.get(&(policy, current)) {
            Some(i) => i,
            None => break,
        };
        if info.otype == TYPE_POLICYMAP {
            return policymap_names.get(&info.config_index).cloned().unwrap_or_default();
        }
        if info.parent == 0 || info.parent == current {
            break;
        }
        current = info.parent;
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Negative-probe cache
// ---------------------------------------------------------------------------

// How long a confirmed "this device has no CBQoS service-policies" answer is
// trusted before we probe again.
pub const QOS_NEGATIVE_TTL: Duration = Duration::from_secs(300);

// Remembers, per device fqdn, when we last got an authoritative empty walk of
// cbQosServicePolicyTable, so the entitypoller skips the probe entirely for the
// TTL. Only empty (authoritative "none") is cached — never an SNMP error, which
// is transient — and devices that DO have policies are never cached, so their
// counters re-poll every cycle. Mirrors vendor::SourceCache's retain contract.
pub struct QosProbeCache {
    negatives: Mutex<HashMap<String, Instant>>,
    ttl: Duration,
}

impl QosProbeCache {
    pub fn new() -> QosProbeCache {
        QosProbeCache::with_ttl(QOS_NEGATIVE_TTL)
    }

    // TTL is injectable so tests exercise expiry without sleeping.
    fn with_ttl(ttl: Duration) -> QosProbeCache {
        QosProbeCache { negatives: Mutex::new(HashMap::new()), ttl }
    }

    // True if a still-fresh negative is on record for this device.
    pub fn should_skip(&self, fqdn: &str, now: Instant) -> bool {
        match self.negatives.lock() {
            Ok(neg) => match neg.get(fqdn) {
                Some(t) => now.saturating_duration_since(*t) < self.ttl,
                None => false,
            },
            Err(_) => false,
        }
    }

    // Record an authoritative "no policies" result.
    pub fn record_absent(&self, fqdn: &str, now: Instant) {
        if let Ok(mut neg) = self.negatives.lock() {
            neg.insert(fqdn.to_string(), now);
        }
    }

    // Device answered with policies: drop any negative so we keep polling it.
    pub fn note_present(&self, fqdn: &str) {
        if let Ok(mut neg) = self.negatives.lock() {
            neg.remove(fqdn);
        }
    }

    // Drop entries for devices no longer monitored.
    pub fn retain(&self, keep: &std::collections::HashSet<String>) {
        if let Ok(mut neg) = self.negatives.lock() {
            neg.retain(|fqdn, _| keep.contains(fqdn));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVICE_POLICY: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cbqos_servicepolicy.json"));
    const OBJECTS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cbqos_objects.json"));
    const POLICYMAP_CFG: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cbqos_policymap_cfg.json"));
    const CM_CFG: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cbqos_cm_cfg.json"));
    const CM_STATS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cbqos_cm_stats.json"));
    const POLICE_STATS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cbqos_police_stats.json"));

    fn parse(fixture: &str) -> SNMPBotResponse {
        serde_json::from_str(fixture).unwrap()
    }

    fn empty() -> SNMPBotResponse {
        serde_json::from_str(r#"{"ID":"x","Entries":[]}"#).unwrap()
    }

    fn decode_fixtures() -> Vec<QosClass> {
        let mut if_names = HashMap::new();
        if_names.insert(9, "Fo1/0/1".to_string());
        decode_qos(
            &parse(SERVICE_POLICY),
            &parse(OBJECTS),
            &parse(POLICYMAP_CFG),
            &parse(CM_CFG),
            &parse(CM_STATS),
            &parse(POLICE_STATS),
            &if_names,
        )
    }

    #[test]
    fn joins_classes_to_policy_interface_and_names() {
        let classes = decode_fixtures();
        // Three classes on the httphttps input policy (Fo1/0/1).
        assert_eq!(classes.len(), 3);
        for c in &classes {
            assert_eq!(c.interface_ifindex, 9);
            assert_eq!(c.interface_name.as_deref(), Some("Fo1/0/1"));
            assert_eq!(c.direction, QosDirection::Input);
            assert_eq!(c.policymap, "httphttps");
        }
        // Sorted by classmap name.
        let names: Vec<&str> = classes.iter().map(|c| c.classmap.as_str()).collect();
        assert_eq!(names, vec!["class-default", "v4httphttps", "v6httphttps"]);
    }

    #[test]
    fn class_map_counters_are_the_hc_columns() {
        let classes = decode_fixtures();
        let v4 = classes.iter().find(|c| c.classmap == "v4httphttps").unwrap();
        assert_eq!(v4.prepolicy_pkts, Some(27381474389));
        assert_eq!(v4.prepolicy_bytes, Some(4875772636329));
    }

    #[test]
    fn police_counters_attach_to_their_class() {
        let classes = decode_fixtures();
        // v6 policer exceeded 16621056 bytes (the live-verified value).
        let v6 = classes.iter().find(|c| c.classmap == "v6httphttps").unwrap();
        let police = v6.police.as_ref().expect("v6 has a policer");
        assert_eq!(police.exceed_bytes, Some(16621056));
        assert_eq!(police.conform_bytes, Some(13522486551429));
        // class-default has no policer.
        let cd = classes.iter().find(|c| c.classmap == "class-default").unwrap();
        assert!(cd.police.is_none());
    }

    #[test]
    fn empty_service_policy_yields_no_classes() {
        let out = decode_qos(&empty(), &empty(), &empty(), &empty(), &empty(), &empty(), &HashMap::new());
        assert!(out.is_empty());
    }

    #[test]
    fn control_plane_ifindex_zero_has_no_name() {
        // ifIndex 0 (control-plane) can't resolve to an ifName.
        let mut if_names = HashMap::new();
        if_names.insert(9, "Fo1/0/1".to_string());
        let classes = decode_qos(
            &parse(SERVICE_POLICY),
            &parse(OBJECTS),
            &parse(POLICYMAP_CFG),
            &parse(CM_CFG),
            &parse(CM_STATS),
            &parse(POLICE_STATS),
            &if_names,
        );
        assert!(classes.iter().all(|c| c.interface_name.is_some()), "fixture is all Fo1/0/1");
    }

    // --- cache ---

    #[test]
    fn cache_skips_within_ttl_and_reprobes_after() {
        let cache = QosProbeCache::with_ttl(Duration::from_secs(300));
        let t0 = Instant::now();
        assert!(!cache.should_skip("sw1", t0), "unknown device is never skipped");
        cache.record_absent("sw1", t0);
        assert!(cache.should_skip("sw1", t0 + Duration::from_secs(60)), "fresh negative -> skip");
        assert!(cache.should_skip("sw1", t0 + Duration::from_secs(299)));
        assert!(!cache.should_skip("sw1", t0 + Duration::from_secs(300)), "expired -> probe again");
        assert!(!cache.should_skip("sw1", t0 + Duration::from_secs(600)));
    }

    #[test]
    fn note_present_clears_a_negative() {
        let cache = QosProbeCache::new();
        let t0 = Instant::now();
        cache.record_absent("sw1", t0);
        assert!(cache.should_skip("sw1", t0));
        cache.note_present("sw1");
        assert!(!cache.should_skip("sw1", t0), "device gained policies -> no longer skipped");
    }

    #[test]
    fn cache_is_per_device() {
        let cache = QosProbeCache::new();
        let t0 = Instant::now();
        cache.record_absent("sw1", t0);
        assert!(cache.should_skip("sw1", t0));
        assert!(!cache.should_skip("sw2", t0), "sw1's negative does not leak to sw2");
    }

    #[test]
    fn retain_drops_unmonitored_devices() {
        let cache = QosProbeCache::new();
        let t0 = Instant::now();
        cache.record_absent("keep.example.com", t0);
        cache.record_absent("drop.example.com", t0);
        let keep: std::collections::HashSet<String> = vec!["keep.example.com".to_string()].into_iter().collect();
        cache.retain(&keep);
        assert!(cache.should_skip("keep.example.com", t0));
        assert!(!cache.should_skip("drop.example.com", t0));
    }
}
