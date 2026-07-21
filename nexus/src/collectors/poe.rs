// Pure decoders for Power-over-Ethernet state. Kept free of I/O (like
// entity_media) so the group.port join and enum handling are unit-tested from
// snmpbot-shaped fixtures; the entitypoller does the SNMP fetches and the
// entPhysicalName->interface resolution, then calls into here.
//
// Two MIB families feed this (both in snmpbot/mibs/):
//   - POWER-ETHERNET-MIB (RFC 3621): vendor-neutral per-port status/class
//     (pethPsePortTable) and switch-wide PSE budget (pethMainPseTable).
//   - CISCO-POWER-ETHERNET-EXT-MIB: per-port watts (consumption/allocated/peak)
//     and the entPhysicalIndex that ties a PSE port back to an interface.
//
// The two per-port tables share the (pethPsePortGroupIndex, pethPsePortIndex)
// index, so decode_port_poe joins them on that key. Enum columns render as
// snmpbot strings (see the SyntaxOptions in the MIB JSON), so status/priority/
// class are read as text and mapped here.

use crate::collectors::entitypoller::{obj_i64, obj_str};
use crate::collectors::poller::SNMPBotResponse;
use std::collections::HashMap;

const GROUP_INDEX: &str = "POWER-ETHERNET-MIB::pethPsePortGroupIndex";
const PORT_INDEX: &str = "POWER-ETHERNET-MIB::pethPsePortIndex";

// pethPsePortDetectionStatus, normalized to the values the UI renders. `Other`
// covers unknown/absent so a firmware quirk never drops a port silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoeStatus {
    Disabled,
    Searching,
    Delivering,
    Fault,
    Test,
    OtherFault,
    Other,
}

impl PoeStatus {
    fn from_snmp(value: &str) -> PoeStatus {
        match value {
            "disabled" => PoeStatus::Disabled,
            "searching" => PoeStatus::Searching,
            "deliveringPower" => PoeStatus::Delivering,
            "fault" => PoeStatus::Fault,
            "test" => PoeStatus::Test,
            "otherFault" => PoeStatus::OtherFault,
            _ => PoeStatus::Other,
        }
    }

    // Stable slug for the API/UI (camelCase to match the SNMP enum names the
    // rest of the codebase surfaces verbatim).
    pub fn as_str(self) -> &'static str {
        match self {
            PoeStatus::Disabled => "disabled",
            PoeStatus::Searching => "searching",
            PoeStatus::Delivering => "deliveringPower",
            PoeStatus::Fault => "fault",
            PoeStatus::Test => "test",
            PoeStatus::OtherFault => "otherFault",
            PoeStatus::Other => "other",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct InterfacePoe {
    pub admin_enabled: bool,
    pub status: PoeStatus,
    pub class: Option<i64>, // 0..=4, decoded from classN
    // Cisco extension only (None on standards-only devices):
    pub power_mw: Option<i64>,     // cpeExtPsePortPwrConsumption (real-time draw)
    pub allocated_mw: Option<i64>, // cpeExtPsePortPwrAllocated
    pub max_drawn_mw: Option<i64>, // cpeExtPsePortMaxPwrDrawn
    pub priority: Option<String>,  // critical/high/low
    // entPhysicalIndex of the PSE port, used by the entitypoller to resolve the
    // owning interface via ENTITY-MIB entPhysicalName. None on the standard path.
    pub ent_phy_index: Option<i64>,
}

// One PSE group's power budget (pethMainPseTable row). A device may report more
// than one PSE (stacked/modular); callers sum or list them.
#[derive(Clone, Debug, PartialEq)]
pub struct PoeBudget {
    pub group: i64,
    pub total_w: i64,    // pethMainPsePower
    pub consumed_w: i64, // pethMainPseConsumptionPower
    pub oper_on: bool,   // pethMainPseOperStatus == on
    // pethMainPseUsageThreshold percent; None when unset (0), which many
    // switches report when no threshold alarm is configured.
    pub threshold_pct: Option<i64>,
}

// "class4" -> Some(4); anything unrecognized -> None.
fn parse_class(value: &str) -> Option<i64> {
    value.strip_prefix("class").and_then(|n| n.parse::<i64>().ok())
}

pub fn decode_main_pse(main: &SNMPBotResponse) -> Vec<PoeBudget> {
    let mut budgets: Vec<PoeBudget> = Vec::new();
    for entry in main.entries.iter() {
        let group = match entry.index.get("POWER-ETHERNET-MIB::pethMainPseGroupIndex") {
            Some(v) => *v,
            None => continue,
        };
        // A PSE row without a power figure isn't useful; skip it rather than
        // report a bogus 0 W budget.
        let total_w = match obj_i64(&entry.objects, "POWER-ETHERNET-MIB::pethMainPsePower") {
            Some(v) => v,
            None => continue,
        };
        let consumed_w = obj_i64(&entry.objects, "POWER-ETHERNET-MIB::pethMainPseConsumptionPower").unwrap_or(0);
        let oper_on = obj_str(&entry.objects, "POWER-ETHERNET-MIB::pethMainPseOperStatus").as_deref() == Some("on");
        let threshold_pct = match obj_i64(&entry.objects, "POWER-ETHERNET-MIB::pethMainPseUsageThreshold") {
            Some(v) if v > 0 => Some(v),
            _ => None,
        };
        budgets.push(PoeBudget { group, total_w, consumed_w, oper_on, threshold_pct });
    }
    budgets.sort_by_key(|b| b.group);
    budgets
}

// Per-port PoE keyed by (group, port). The standard pethPsePortTable is
// authoritative for which ports exist; the Cisco cpeExtPsePortTable (when
// present) overlays watts + entPhyIndex on the same index.
pub fn decode_port_poe(
    peth: &SNMPBotResponse,
    cpext: Option<&SNMPBotResponse>,
) -> HashMap<(i64, i64), InterfacePoe> {
    let mut ports: HashMap<(i64, i64), InterfacePoe> = HashMap::new();
    for entry in peth.entries.iter() {
        let key = match port_key(entry) {
            Some(k) => k,
            None => continue,
        };
        let admin_enabled = obj_str(&entry.objects, "POWER-ETHERNET-MIB::pethPsePortAdminEnable").as_deref() == Some("true");
        let status = obj_str(&entry.objects, "POWER-ETHERNET-MIB::pethPsePortDetectionStatus")
            .map(|s| PoeStatus::from_snmp(&s))
            .unwrap_or(PoeStatus::Other);
        let class = obj_str(&entry.objects, "POWER-ETHERNET-MIB::pethPsePortPowerClassifications")
            .and_then(|c| parse_class(&c));
        let priority = obj_str(&entry.objects, "POWER-ETHERNET-MIB::pethPsePortPowerPriority");
        ports.insert(key, InterfacePoe {
            admin_enabled,
            status,
            class,
            power_mw: None,
            allocated_mw: None,
            max_drawn_mw: None,
            priority,
            ent_phy_index: None,
        });
    }

    for entry in cpext.map(|c| c.entries.iter()).into_iter().flatten() {
        let key = match port_key(entry) {
            Some(k) => k,
            None => continue,
        };
        // Only overlay onto a port the standard table already reported; a Cisco
        // row without a standard row would have no status/class to show.
        if let Some(poe) = ports.get_mut(&key) {
            poe.power_mw = obj_i64(&entry.objects, "CISCO-POWER-ETHERNET-EXT-MIB::cpeExtPsePortPwrConsumption");
            poe.allocated_mw = obj_i64(&entry.objects, "CISCO-POWER-ETHERNET-EXT-MIB::cpeExtPsePortPwrAllocated");
            poe.max_drawn_mw = obj_i64(&entry.objects, "CISCO-POWER-ETHERNET-EXT-MIB::cpeExtPsePortMaxPwrDrawn");
            poe.ent_phy_index = obj_i64(&entry.objects, "CISCO-POWER-ETHERNET-EXT-MIB::cpeExtPsePortEntPhyIndex");
        }
    }
    ports
}

fn port_key(entry: &crate::collectors::poller::SNMPBotResultEntry) -> Option<(i64, i64)> {
    match (entry.index.get(GROUP_INDEX), entry.index.get(PORT_INDEX)) {
        (Some(g), Some(p)) => Some((*g, *p)),
        _ => None,
    }
}

// Resolve per-port PoE to db interface ids for the API overlay. Pure so the
// entPhyIndex -> entPhysicalName -> interface chain (the same join entity_media
// uses for media) is testable without SNMP. `phys_names` is entPhysicalIndex ->
// entPhysicalName; `iface_by_name` maps that name (or ifDescr) to interface id.
// Ports whose entPhyIndex doesn't resolve (no Cisco extension, or an unmapped
// name) are dropped — the switch-wide budget still surfaces without them.
pub fn map_to_interfaces(
    ports: &HashMap<(i64, i64), InterfacePoe>,
    phys_names: &HashMap<i64, String>,
    iface_by_name: &HashMap<String, i32>,
) -> HashMap<i32, InterfacePoe> {
    let mut out: HashMap<i32, InterfacePoe> = HashMap::new();
    for poe in ports.values() {
        let phy = match poe.ent_phy_index {
            Some(v) => v,
            None => continue,
        };
        let name = match phys_names.get(&phy) {
            Some(n) => n,
            None => continue,
        };
        if let Some(id) = iface_by_name.get(name) {
            out.insert(*id, poe.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pethmainpsetable.json"));
    const PORTS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pethpseporttable.json"));
    const CISCO: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cpeextpseporttable.json"));

    fn parse(fixture: &str) -> SNMPBotResponse {
        serde_json::from_str(fixture).unwrap()
    }

    #[test]
    fn parse_class_strips_prefix() {
        assert_eq!(parse_class("class0"), Some(0));
        assert_eq!(parse_class("class4"), Some(4));
        assert_eq!(parse_class("garbage"), None);
        assert_eq!(parse_class(""), None);
    }

    #[test]
    fn main_pse_decodes_budget_and_threshold() {
        let budgets = decode_main_pse(&parse(MAIN));
        assert_eq!(budgets.len(), 1);
        let b = &budgets[0];
        assert_eq!(b.group, 1);
        assert_eq!(b.total_w, 124);
        assert_eq!(b.consumed_w, 10);
        assert!(b.oper_on);
        // Fixture threshold 0 -> unset.
        assert_eq!(b.threshold_pct, None);
    }

    #[test]
    fn port_poe_decodes_status_class_from_standard_table() {
        let ports = decode_port_poe(&parse(PORTS), None);
        assert_eq!(ports.len(), 8);
        // Port 1 delivering, class 4 (live ticket-sw2 shape).
        let p1 = &ports[&(1, 1)];
        assert_eq!(p1.status, PoeStatus::Delivering);
        assert_eq!(p1.class, Some(4));
        assert!(p1.admin_enabled);
        assert_eq!(p1.priority.as_deref(), Some("low"));
        // No Cisco table -> no watts.
        assert_eq!(p1.power_mw, None);
        // Port 3 searching, class 0.
        assert_eq!(ports[&(1, 3)].status, PoeStatus::Searching);
        assert_eq!(ports[&(1, 3)].class, Some(0));
        // Port 6 admin-disabled.
        assert_eq!(ports[&(1, 6)].status, PoeStatus::Disabled);
    }

    #[test]
    fn cisco_overlay_adds_watts_and_phyindex() {
        let ports = decode_port_poe(&parse(PORTS), Some(&parse(CISCO)));
        let p1 = &ports[&(1, 1)];
        assert_eq!(p1.power_mw, Some(4578));
        assert_eq!(p1.allocated_mw, Some(15400));
        assert_eq!(p1.max_drawn_mw, Some(5229));
        assert_eq!(p1.ent_phy_index, Some(1005));
        // A non-delivering port still gets its (zero) Cisco figures.
        assert_eq!(ports[&(1, 3)].power_mw, Some(0));
        assert_eq!(ports[&(1, 3)].ent_phy_index, Some(1007));
    }

    #[test]
    fn map_to_interfaces_joins_via_entphysname() {
        let ports = decode_port_poe(&parse(PORTS), Some(&parse(CISCO)));
        // entPhyIndex 1005 -> "GigabitEthernet0/1" -> interface id 501.
        let mut phys_names = HashMap::new();
        phys_names.insert(1005, "GigabitEthernet0/1".to_string());
        phys_names.insert(1006, "GigabitEthernet0/2".to_string());
        let mut iface_by_name = HashMap::new();
        iface_by_name.insert("GigabitEthernet0/1".to_string(), 501);
        iface_by_name.insert("GigabitEthernet0/2".to_string(), 502);

        let resolved = map_to_interfaces(&ports, &phys_names, &iface_by_name);
        // Only ports 1 and 2 have resolvable names; the rest drop out.
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[&501].status, PoeStatus::Delivering);
        assert_eq!(resolved[&501].power_mw, Some(4578));
        assert_eq!(resolved[&502].power_mw, Some(1635));
    }

    #[test]
    fn map_to_interfaces_drops_ports_without_cisco_extension() {
        // Standards-only device: no entPhyIndex anywhere -> nothing resolves.
        let ports = decode_port_poe(&parse(PORTS), None);
        let resolved = map_to_interfaces(&ports, &HashMap::new(), &HashMap::new());
        assert!(resolved.is_empty());
    }
}
