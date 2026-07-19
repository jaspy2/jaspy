// Derive per-interface physical media / form-factor from ENTITY-MIB.
//
// IF-MIB::ifType reports every physical Ethernet port as "ethernetCsmacd"
// (copper and fiber alike), so it cannot tell an RJ45 port from an SFP cage.
// ENTITY-MIB::entPhysicalTable can: on Cisco (verified live on a 2960CX) a
// fixed port is entPhysicalClass "port" whose name is the ifName
// (e.g. "GigabitEthernet0/1"), while an SFP cage is entPhysicalClass
// "container" named "<ifName> Container". A plugged transceiver appears as a
// "module" row contained in the cage (entPhysicalContainedIn == the container's
// entPhysicalIndex), carrying the optic in its descr/model.
//
// We key the result by entity name (== ifName / ifDescr) and let callers join
// to their interface list: ENTITY-MIB::entAliasMappingTable is the "clean"
// OID-based entPhysicalIndex->ifIndex join but is unusable through snmpbot
// (v0.1.0 fails to decode its OID-valued rows), so name matching is the same
// technique the entitypoller already uses to attach sensors to interfaces.

use std::collections::HashMap;

use crate::collectors::poller::{SNMPBotResultEntry, SNMPBotResultEntryObjectValue};

const CLASS: &str = "ENTITY-MIB::entPhysicalClass";
const NAME: &str = "ENTITY-MIB::entPhysicalName";
const DESCR: &str = "ENTITY-MIB::entPhysicalDescr";
const MODEL: &str = "ENTITY-MIB::entPhysicalModelName";
const CONTAINED_IN: &str = "ENTITY-MIB::entPhysicalContainedIn";
const INDEX: &str = "ENTITY-MIB::entPhysicalIndex";

const CONTAINER_SUFFIX: &str = " Container";

fn obj_str(objects: &HashMap<String, SNMPBotResultEntryObjectValue>, key: &str) -> Option<String> {
    match objects.get(key) {
        Some(SNMPBotResultEntryObjectValue::Str(v)) => {
            let v = v.trim();
            if v.is_empty() { None } else { Some(v.to_string()) }
        },
        _ => None,
    }
}

fn obj_i64(objects: &HashMap<String, SNMPBotResultEntryObjectValue>, key: &str) -> Option<i64> {
    match objects.get(key) {
        Some(SNMPBotResultEntryObjectValue::Uint64(v)) => Some(*v as i64),
        Some(SNMPBotResultEntryObjectValue::Float64(v)) => Some(*v as i64),
        _ => None,
    }
}

// Media values are plain strings so they persist trivially and the UI can bucket
// them: "copper" (fixed RJ45 port), "sfp" (empty cage), "sfp: <descr>"
// (populated transceiver). Callers treat a `sfp` prefix as an SFP/SFP+ slot.
pub fn media_copper() -> String { "copper".to_string() }
pub fn media_sfp_empty() -> String { "sfp".to_string() }
pub fn media_sfp_populated(descr: &str) -> String { format!("sfp: {}", descr) }

/// Classify each physical port entity into a media/form-factor string, keyed by
/// entity name (== ifName).
///
/// The key discriminator, verified against real Cisco IOS-XE (C9300): a plugged
/// transceiver is reported as `entPhysicalClass "port"` whose parent
/// (`entPhysicalContainedIn`) is a **container** (the SFP cage), with the optic
/// media in its `entPhysicalDescr` (e.g. "SFP-10GBase-LR", "1000BaseLX SFP").
/// A fixed RJ45 port is also `class "port"` but its parent is a **module** /
/// chassis, and its descr is just the port name. An **empty** cage is a
/// `container` with no port child.
pub fn classify_media(entries: &[SNMPBotResultEntry]) -> HashMap<String, String> {
    // entPhysicalIndex -> class, so a port can tell whether its parent is a cage.
    let mut class_by_index: HashMap<i64, String> = HashMap::new();
    for entry in entries {
        if let (Some(idx), Some(class)) = (entry.index.get(INDEX), obj_str(&entry.objects, CLASS)) {
            class_by_index.insert(*idx, class);
        }
    }
    let is_container = |idx: i64| class_by_index.get(&idx).map(|c| c == "container").unwrap_or(false);

    // Containers that hold any child entity are populated cages.
    let mut container_has_child: HashMap<i64, bool> = HashMap::new();
    for entry in entries {
        if let Some(parent) = obj_i64(&entry.objects, CONTAINED_IN) {
            if is_container(parent) {
                container_has_child.insert(parent, true);
            }
        }
    }

    let mut media: HashMap<String, String> = HashMap::new();

    // Ports: parent is a cage -> populated transceiver (descr = optic media);
    // otherwise a fixed port -> copper.
    for entry in entries {
        if obj_str(&entry.objects, CLASS).as_deref() != Some("port") {
            continue;
        }
        let name = match obj_str(&entry.objects, NAME) {
            Some(n) => n,
            None => continue,
        };
        let parent_is_cage = obj_i64(&entry.objects, CONTAINED_IN).map(is_container).unwrap_or(false);
        if parent_is_cage {
            let descr = obj_str(&entry.objects, DESCR).or_else(|| obj_str(&entry.objects, MODEL));
            match descr {
                Some(descr) => media.insert(name, media_sfp_populated(&descr)),
                None => media.insert(name, media_sfp_empty()),
            };
        } else {
            media.insert(name, media_copper());
        }
    }

    // Empty cages: a container named "<iface> Container" with no child port.
    for entry in entries {
        if obj_str(&entry.objects, CLASS).as_deref() != Some("container") {
            continue;
        }
        let name = match obj_str(&entry.objects, NAME) {
            Some(n) => n,
            None => continue,
        };
        let iface_name = match name.strip_suffix(CONTAINER_SUFFIX) {
            // Interface names carry no spaces; this drops PSU/fan/FRU cages
            // ("Switch 1 - Fan 1 Container") that also end in " Container".
            Some(stripped) if !stripped.is_empty() && !stripped.contains(' ') => stripped.to_string(),
            _ => continue,
        };
        let has_child = entry.index.get(INDEX)
            .map(|i| container_has_child.get(i).copied().unwrap_or(false))
            .unwrap_or(false);
        if !has_child {
            media.entry(iface_name).or_insert_with(media_sfp_empty);
        }
    }

    media
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Build an entry with a given index + object map, matching the snmpbot
    // shape the collectors deserialize.
    fn entry(index: i64, objects: serde_json::Value) -> SNMPBotResultEntry {
        let objects: HashMap<String, SNMPBotResultEntryObjectValue> =
            serde_json::from_value(objects).unwrap();
        let mut index_map = HashMap::new();
        index_map.insert(INDEX.to_string(), index);
        SNMPBotResultEntry { host_i_d: "test".to_string(), index: index_map, objects }
    }

    // Shapes below mirror a real Cisco C9300-24P (tele-sw1) read via snmpbot:
    // a fixed module (1060) holding copper ports, and an 8x10G uplink module
    // (1086) holding per-port containers (1087+), each populated by a class
    // "port" transceiver whose descr is the optic media.
    #[test]
    fn fixed_port_is_copper() {
        let entries = vec![
            entry(1060, json!({ CLASS: "module", NAME: "Fixed Module 0" })),
            entry(1062, json!({ CLASS: "port", NAME: "Gi1/0/1", DESCR: "Gi1/0/1", CONTAINED_IN: 1060u64 })),
        ];
        let media = classify_media(&entries);
        assert_eq!(media.get("Gi1/0/1").map(String::as_str), Some("copper"));
    }

    #[test]
    fn empty_cage_is_sfp() {
        let entries = vec![entry(1089, json!({
            CLASS: "container",
            NAME: "Te1/1/3 Container",
            DESCR: "Te1/1/3 Container",
        }))];
        let media = classify_media(&entries);
        assert_eq!(media.get("Te1/1/3").map(String::as_str), Some("sfp"));
    }

    #[test]
    fn populated_cage_reports_optic_descr() {
        // A 10G optic (SFP+) and a 1G optic (SFP) plugged into 10G-capable cages.
        let entries = vec![
            entry(1087, json!({ CLASS: "container", NAME: "Te1/1/1 Container" })),
            entry(1095, json!({ CLASS: "port", NAME: "Te1/1/1", DESCR: "SFP-10GBase-LR", CONTAINED_IN: 1087u64 })),
            entry(1093, json!({ CLASS: "container", NAME: "Te1/1/7 Container" })),
            entry(1100, json!({ CLASS: "port", NAME: "Te1/1/7", DESCR: "1000BaseLX SFP", CONTAINED_IN: 1093u64 })),
        ];
        let media = classify_media(&entries);
        assert_eq!(media.get("Te1/1/1").map(String::as_str), Some("sfp: SFP-10GBase-LR"));
        assert_eq!(media.get("Te1/1/7").map(String::as_str), Some("sfp: 1000BaseLX SFP"));
    }

    // Classify the real, unmodified entPhysicalTable captured from a production
    // Cisco C9300-24P (tele-sw1) via snmpbot — the capture that corrected the
    // "optic is class=module" assumption. Guards against real-world drift.
    #[test]
    fn classifies_real_c9300_capture() {
        use crate::collectors::poller::SNMPBotResponse;
        let raw = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/entphysicaltable_c9300.json"));
        let resp: SNMPBotResponse = serde_json::from_str(raw).unwrap();
        let media = classify_media(&resp.entries);

        // 24 fixed copper ports.
        for n in 1..=24 {
            assert_eq!(media.get(&format!("Gi1/0/{}", n)).map(String::as_str), Some("copper"), "Gi1/0/{}", n);
        }
        // Populated optics carry the real descr (10G = SFP+, 1G = SFP).
        assert_eq!(media.get("Te1/1/1").map(String::as_str), Some("sfp: SFP-10GBase-LR"));
        assert_eq!(media.get("Te1/1/2").map(String::as_str), Some("sfp: SFP-10GBase-SR"));
        assert_eq!(media.get("Te1/1/5").map(String::as_str), Some("sfp: SFP-10GBase-LR"));
        assert_eq!(media.get("Te1/1/8").map(String::as_str), Some("sfp: SFP-10GBase-SR"));
        // A 1G optic seated in a 10G cage — must read as SFP (1G), not SFP+.
        assert_eq!(media.get("Te1/1/7").map(String::as_str), Some("sfp: 1000BaseLX SFP"));
        // Empty cages.
        for n in [3, 4, 6] {
            assert_eq!(media.get(&format!("Te1/1/{}", n)).map(String::as_str), Some("sfp"), "Te1/1/{}", n);
        }
        // PSU / fan / FRULink containers must not surface as interfaces.
        assert!(media.keys().all(|k| !k.contains(' ')), "junk keys: {:?}", media.keys().collect::<Vec<_>>());
        // Exactly the 24 copper + 8 SFP cages.
        assert_eq!(media.len(), 32, "media: {:?}", media);
    }

    #[test]
    fn non_interface_entities_are_ignored() {
        let entries = vec![
            entry(1000, json!({ CLASS: "chassis", NAME: "Switch 1", DESCR: "C9300-24P" })),
            entry(1006, json!({ CLASS: "container", NAME: "Switch 1 - Power Supply A Container" })),
            entry(1009, json!({ CLASS: "container", NAME: "Switch 1 - Fan 1 Container" })),
            entry(1060, json!({ CLASS: "module", NAME: "Fixed Module 0", CONTAINED_IN: 1000u64 })),
            entry(3, json!({ CLASS: "sensor", NAME: "Gi1/0/1 Temperature Sensor" })),
        ];
        let media = classify_media(&entries);
        // PSU/fan cages have spaces in the stripped name and must not appear.
        assert!(media.is_empty(), "unexpected media: {:?}", media);
    }
}
