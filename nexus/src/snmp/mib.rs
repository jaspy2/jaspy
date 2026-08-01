// MIB registry: loads the snmpbot JSON MIB files (snmpbot/mibs/*.json) so the
// embedded client resolves the same `MIB::name` object/table identifiers the
// collectors already use — including the custom `jaspy*` slim-view tables that
// exist only in those JSON files. This gives name->OID and value-syntax parity
// with snmpbot for free: we ship and read the exact same definitions.
//
// The snmpbot JSON schema (verified against the checked-in files):
//   { "Name": "IF-MIB", "OID": ".1.3.6.1.2.1.31",
//     "Objects": [ { "Name": "ifIndex", "OID": ".1.3...", "Syntax": "Integer32",
//                    "SyntaxOptions": <array for ENUM/BITS | object for sized strings> }, ... ],
//     "Tables":  [ { "Name": "ifTable", "OID": ".1.3...", "EntryName": "ifEntry",
//                    "IndexObjects": ["IF-MIB::ifIndex"] | "AugmentsEntry": "IF-MIB::ifEntry",
//                    "EntryObjects": ["IF-MIB::ifIndex", ...] } ] }
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

// --- resolved model -------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Syntax {
    Integer,       // INTEGER / Integer32 (signed)
    Unsigned,      // Counter32/64, Gauge32, Unsigned32, TimeTicks
    OctetString,   // raw octets -> space-separated lowercase hex
    DisplayString, // human-readable text
    MacAddress,    // 6 octets -> aa:bb:cc:dd:ee:ff
    IpAddress,
    ObjectIdentifier,
    Enum(HashMap<i64, String>),
    Bits(Vec<(u32, String)>),
}

#[derive(Debug)]
pub struct MibObject {
    pub id: String, // "IF-MIB::ifIndex"
    pub oid: Vec<u64>,
    pub syntax: Syntax,
}

#[derive(Debug)]
pub struct MibTable {
    pub id: String, // "IF-MIB::ifTable"
    #[allow(dead_code)]
    pub oid: Vec<u64>,
    pub index: Vec<Arc<MibObject>>,
    pub columns: Vec<Arc<MibObject>>,
}

pub struct MibRegistry {
    objects: HashMap<String, Arc<MibObject>>,
    tables: HashMap<String, Arc<MibTable>>,
}

// --- raw JSON model -------------------------------------------------------

#[derive(Deserialize)]
struct RawMib {
    #[serde(rename = "Name")]
    name: String,
    #[serde(default, rename = "Objects")]
    objects: Vec<RawObject>,
    #[serde(default, rename = "Tables")]
    tables: Vec<RawTable>,
}

#[derive(Deserialize)]
struct RawObject {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "OID")]
    oid: String,
    #[serde(default, rename = "Syntax")]
    syntax: Option<String>,
    #[serde(default, rename = "SyntaxOptions")]
    syntax_options: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawTable {
    #[serde(rename = "Name")]
    name: String,
    #[serde(default, rename = "OID")]
    oid: Option<String>,
    #[serde(default, rename = "EntryName")]
    entry_name: Option<String>,
    #[serde(default, rename = "IndexObjects")]
    index_objects: Option<Vec<String>>,
    #[serde(default, rename = "AugmentsEntry")]
    augments_entry: Option<String>,
    #[serde(default, rename = "EntryObjects")]
    entry_objects: Vec<String>,
}

// Enum/BITS SyntaxOptions element: {"Name": "up", "Value": 1} or
// {"Name": "lacpActivity", "Bit": 0}.
#[derive(Deserialize)]
struct RawSyntaxOption {
    #[serde(rename = "Name")]
    name: String,
    #[serde(default, rename = "Value")]
    value: Option<i64>,
    #[serde(default, rename = "Bit")]
    bit: Option<u32>,
}

// --- OID parsing ----------------------------------------------------------

pub fn parse_oid(dotted: &str) -> Option<Vec<u64>> {
    let trimmed = dotted.trim().trim_start_matches('.');
    if trimmed.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for part in trimmed.split('.') {
        match part.parse::<u64>() {
            Ok(v) => out.push(v),
            Err(_) => return None,
        }
    }
    Some(out)
}

// --- syntax classification ------------------------------------------------

fn classify_syntax(raw: &RawObject) -> Syntax {
    let syntax = raw.syntax.as_deref().unwrap_or("");
    // Strip a "MIB::" qualifier so both "DisplayString" and
    // "SNMPv2-TC::DisplayString" classify identically.
    let short = syntax.rsplit("::").next().unwrap_or(syntax);

    match short {
        "ENUM" => Syntax::Enum(parse_enum_options(raw.syntax_options.as_ref())),
        "BITS" => Syntax::Bits(parse_bits_options(raw.syntax_options.as_ref())),
        "Integer32" | "INTEGER" | "Integer" => Syntax::Integer,
        "Counter32" | "Counter64" | "Counter" | "Gauge32" | "Gauge" | "Unsigned32"
        | "TimeTicks" | "TimeInterval" => Syntax::Unsigned,
        "DisplayString" | "SnmpAdminString" | "OwnerString" => Syntax::DisplayString,
        "MacAddress" | "PhysAddress" => Syntax::MacAddress,
        "IpAddress" | "InetAddressIPv4" | "NetworkAddress" => Syntax::IpAddress,
        "OBJECT IDENTIFIER" | "ObjectIdentifier" | "AutonomousType" | "RowPointer"
        | "VariablePointer" | "InstancePointer" => Syntax::ObjectIdentifier,
        // "OCTET STRING" and any sized/opaque/unknown octet type render as raw
        // hex, matching snmpbot's default for unnamed octet strings.
        _ => Syntax::OctetString,
    }
}

fn parse_enum_options(options: Option<&serde_json::Value>) -> HashMap<i64, String> {
    let mut map = HashMap::new();
    if let Some(serde_json::Value::Array(items)) = options {
        for item in items {
            if let Ok(opt) = serde_json::from_value::<RawSyntaxOption>(item.clone()) {
                if let Some(value) = opt.value {
                    map.insert(value, opt.name);
                }
            }
        }
    }
    map
}

fn parse_bits_options(options: Option<&serde_json::Value>) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    if let Some(serde_json::Value::Array(items)) = options {
        for item in items {
            if let Ok(opt) = serde_json::from_value::<RawSyntaxOption>(item.clone()) {
                if let Some(bit) = opt.bit {
                    out.push((bit, opt.name));
                }
            }
        }
    }
    out.sort_by_key(|(bit, _)| *bit);
    out
}

// --- loading --------------------------------------------------------------

impl MibRegistry {
    // Load every *.json in `dir`. An empty or unreadable directory is an error
    // (the caller — embedded mode startup — treats it as fatal; a silent empty
    // registry would look like a total network outage).
    pub fn load(dir: &Path) -> Result<MibRegistry, String> {
        let entries = std::fs::read_dir(dir).map_err(|e| format!("cannot read mib dir {}: {}", dir.display(), e))?;
        let mut raw_mibs: Vec<RawMib> = Vec::new();
        let mut files_read = 0usize;
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    println!("[mib] skipping {}: {}", path.display(), e);
                    continue;
                }
            };
            match serde_json::from_str::<RawMib>(&text) {
                Ok(mib) => {
                    files_read += 1;
                    raw_mibs.push(mib);
                }
                Err(e) => println!("[mib] skipping {}: parse error {}", path.display(), e),
            }
        }
        if files_read == 0 {
            return Err(format!("no usable MIB JSON files found in {}", dir.display()));
        }
        Ok(Self::from_raw(raw_mibs))
    }

    fn from_raw(raw_mibs: Vec<RawMib>) -> MibRegistry {
        let mut objects: HashMap<String, Arc<MibObject>> = HashMap::new();
        // Map "MIB::EntryName" -> the raw table, for AUGMENTS resolution.
        let mut entry_index: HashMap<String, (String, RawTable)> = HashMap::new();

        // Pass 1: objects and an entry-name index.
        for mib in &raw_mibs {
            for obj in &mib.objects {
                let oid = match parse_oid(&obj.oid) {
                    Some(o) => o,
                    None => continue,
                };
                let id = format!("{}::{}", mib.name, obj.name);
                objects.insert(
                    id.clone(),
                    Arc::new(MibObject { id, oid, syntax: classify_syntax(obj) }),
                );
            }
        }

        // Collect tables (clone into an owned list; we need two passes and the
        // raw structs aren't Clone, so index them by moving).
        let mut all_tables: Vec<(String, RawTable)> = Vec::new();
        for mib in raw_mibs {
            let mib_name = mib.name;
            for table in mib.tables {
                if let Some(entry_name) = &table.entry_name {
                    entry_index.insert(format!("{}::{}", mib_name, entry_name), (mib_name.clone(), clone_raw_table(&table)));
                }
                all_tables.push((mib_name.clone(), table));
            }
        }

        // Pass 2: resolve tables, following AUGMENTS for the index.
        let mut tables: HashMap<String, Arc<MibTable>> = HashMap::new();
        for (mib_name, table) in &all_tables {
            let id = format!("{}::{}", mib_name, table.name);
            let oid = table.oid.as_deref().and_then(parse_oid).unwrap_or_default();

            let index_ids = match resolve_index_ids(table, &entry_index) {
                Some(ids) => ids,
                None => {
                    println!("[mib] table {} has no resolvable index, skipping", id);
                    continue;
                }
            };

            let mut index = Vec::new();
            let mut ok = true;
            for iid in &index_ids {
                match objects.get(iid) {
                    Some(o) => index.push(o.clone()),
                    None => {
                        println!("[mib] table {} index object {} unknown, skipping table", id, iid);
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }

            // Columns = entry objects that are not index objects (indexes are
            // encoded in the OID suffix, not walked as columns). Unknown column
            // objects are dropped individually.
            let mut columns = Vec::new();
            for cid in &table.entry_objects {
                if index_ids.contains(cid) {
                    continue;
                }
                if let Some(o) = objects.get(cid) {
                    columns.push(o.clone());
                }
            }

            tables.insert(id.clone(), Arc::new(MibTable { id, oid, index, columns }));
        }

        MibRegistry { objects, tables }
    }

    pub fn table(&self, id: &str) -> Option<&Arc<MibTable>> {
        self.tables.get(id)
    }

    pub fn object(&self, id: &str) -> Option<&Arc<MibObject>> {
        self.objects.get(id)
    }

    pub fn object_count(&self) -> usize {
        self.objects.len()
    }

    pub fn table_count(&self) -> usize {
        self.tables.len()
    }
}

// The index objects for a table: its own IndexObjects, or (for an AUGMENTS
// table) the index of the entry it augments, resolved transitively.
fn resolve_index_ids(table: &RawTable, entry_index: &HashMap<String, (String, RawTable)>) -> Option<Vec<String>> {
    if let Some(ids) = &table.index_objects {
        if !ids.is_empty() {
            return Some(ids.clone());
        }
    }
    let mut augments = table.augments_entry.clone();
    // Bound the walk so a malformed self-referential AUGMENTS can't loop.
    for _ in 0..16 {
        let target = augments?;
        let (_, augmented) = entry_index.get(&target)?;
        if let Some(ids) = &augmented.index_objects {
            if !ids.is_empty() {
                return Some(ids.clone());
            }
        }
        augments = augmented.augments_entry.clone();
    }
    None
}

fn clone_raw_table(t: &RawTable) -> RawTable {
    RawTable {
        name: t.name.clone(),
        oid: t.oid.clone(),
        entry_name: t.entry_name.clone(),
        index_objects: t.index_objects.clone(),
        augments_entry: t.augments_entry.clone(),
        entry_objects: t.entry_objects.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg_from_json(jsons: &[&str]) -> MibRegistry {
        let raws: Vec<RawMib> = jsons.iter().map(|j| serde_json::from_str(j).unwrap()).collect();
        MibRegistry::from_raw(raws)
    }

    #[test]
    fn parse_oid_variants() {
        assert_eq!(parse_oid(".1.3.6.1"), Some(vec![1, 3, 6, 1]));
        assert_eq!(parse_oid("1.3.6"), Some(vec![1, 3, 6]));
        assert_eq!(parse_oid(""), None);
        assert_eq!(parse_oid(".1.x.3"), None);
    }

    #[test]
    fn enum_and_bits_and_mac_classify() {
        let reg = reg_from_json(&[r#"{
            "Name": "T",
            "Objects": [
                {"Name": "st", "OID": ".1.1.1", "Syntax": "ENUM", "SyntaxOptions": [{"Name":"up","Value":1},{"Name":"down","Value":2}]},
                {"Name": "bits", "OID": ".1.1.2", "Syntax": "BITS", "SyntaxOptions": [{"Name":"a","Bit":0},{"Name":"b","Bit":2}]},
                {"Name": "mac", "OID": ".1.1.3", "Syntax": "SNMPv2-TC::MacAddress"},
                {"Name": "descr", "OID": ".1.1.4", "Syntax": "SNMPv2-TC::DisplayString"},
                {"Name": "raw", "OID": ".1.1.5", "Syntax": "OCTET STRING", "SyntaxOptions": {"Min":128,"Max":128}},
                {"Name": "ctr", "OID": ".1.1.6", "Syntax": "Counter64"}
            ]
        }"#]);
        assert!(matches!(reg.object("T::st").unwrap().syntax, Syntax::Enum(_)));
        assert!(matches!(reg.object("T::bits").unwrap().syntax, Syntax::Bits(_)));
        assert!(matches!(reg.object("T::mac").unwrap().syntax, Syntax::MacAddress));
        assert!(matches!(reg.object("T::descr").unwrap().syntax, Syntax::DisplayString));
        assert!(matches!(reg.object("T::raw").unwrap().syntax, Syntax::OctetString));
        assert!(matches!(reg.object("T::ctr").unwrap().syntax, Syntax::Unsigned));
        if let Syntax::Enum(m) = &reg.object("T::st").unwrap().syntax {
            assert_eq!(m.get(&1).map(String::as_str), Some("up"));
        }
    }

    #[test]
    fn table_resolves_index_and_columns() {
        let reg = reg_from_json(&[r#"{
            "Name": "IF-MIB",
            "Objects": [
                {"Name": "ifIndex", "OID": ".1.3.6.1.2.1.2.2.1.1", "Syntax": "Integer32"},
                {"Name": "ifDescr", "OID": ".1.3.6.1.2.1.2.2.1.2", "Syntax": "SNMPv2-TC::DisplayString"}
            ],
            "Tables": [
                {"Name": "ifTable", "OID": ".1.3.6.1.2.1.2.2", "EntryName": "ifEntry",
                 "IndexObjects": ["IF-MIB::ifIndex"],
                 "EntryObjects": ["IF-MIB::ifIndex", "IF-MIB::ifDescr"]}
            ]
        }"#]);
        let table = reg.table("IF-MIB::ifTable").unwrap();
        assert_eq!(table.index.len(), 1);
        assert_eq!(table.index[0].id, "IF-MIB::ifIndex");
        // ifIndex is an index, so it's not also walked as a column.
        assert_eq!(table.columns.len(), 1);
        assert_eq!(table.columns[0].id, "IF-MIB::ifDescr");
    }

    #[test]
    fn augments_inherits_index() {
        // ifXTable AUGMENTS ifEntry and has no IndexObjects of its own.
        let reg = reg_from_json(&[r#"{
            "Name": "IF-MIB",
            "Objects": [
                {"Name": "ifIndex", "OID": ".1.3.6.1.2.1.2.2.1.1", "Syntax": "Integer32"},
                {"Name": "ifName", "OID": ".1.3.6.1.2.1.31.1.1.1.1", "Syntax": "SNMPv2-TC::DisplayString"}
            ],
            "Tables": [
                {"Name": "ifTable", "OID": ".1.3.6.1.2.1.2.2", "EntryName": "ifEntry",
                 "IndexObjects": ["IF-MIB::ifIndex"], "EntryObjects": ["IF-MIB::ifIndex"]},
                {"Name": "ifXTable", "OID": ".1.3.6.1.2.1.31.1.1", "EntryName": "ifXEntry",
                 "AugmentsEntry": "IF-MIB::ifEntry", "EntryObjects": ["IF-MIB::ifName"]}
            ]
        }"#]);
        let xtable = reg.table("IF-MIB::ifXTable").unwrap();
        assert_eq!(xtable.index.len(), 1);
        assert_eq!(xtable.index[0].id, "IF-MIB::ifIndex");
        assert_eq!(xtable.columns[0].id, "IF-MIB::ifName");
    }

    #[test]
    fn cross_mib_index_reference_resolves() {
        // ENTITY-SENSOR-MIB's entPhySensorTable is indexed by an ENTITY-MIB
        // object; both MIBs must be loaded together to resolve.
        let reg = reg_from_json(&[
            r#"{"Name": "ENTITY-MIB", "Objects": [{"Name": "entPhysicalIndex", "OID": ".1.3.6.1.2.1.47.1.1.1.1.1", "Syntax": "Integer32"}]}"#,
            r#"{"Name": "ENTITY-SENSOR-MIB",
                "Objects": [{"Name": "entPhySensorValue", "OID": ".1.3.6.1.2.1.99.1.1.1.4", "Syntax": "Integer32"}],
                "Tables": [{"Name": "entPhySensorTable", "OID": ".1.3.6.1.2.1.99.1.1",
                            "EntryName": "entPhySensorEntry", "IndexObjects": ["ENTITY-MIB::entPhysicalIndex"],
                            "EntryObjects": ["ENTITY-SENSOR-MIB::entPhySensorValue"]}]}"#,
        ]);
        let table = reg.table("ENTITY-SENSOR-MIB::entPhySensorTable").unwrap();
        assert_eq!(table.index[0].id, "ENTITY-MIB::entPhysicalIndex");
        assert_eq!(table.columns[0].id, "ENTITY-SENSOR-MIB::entPhySensorValue");
    }

    // Loads the real checked-in MIB directory when present (skipped in
    // packaged builds without the repo layout). Guards the parity contract:
    // every table/object the collectors request by name must resolve.
    fn real_registry() -> Option<MibRegistry> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../snmpbot/mibs");
        if !dir.is_dir() {
            return None;
        }
        Some(MibRegistry::load(&dir).unwrap())
    }

    #[test]
    fn real_mibs_resolve_every_collector_table() {
        let reg = match real_registry() {
            Some(r) => r,
            None => return,
        };
        let tables = [
            "IF-MIB::ifTable",
            "IF-MIB::ifXTable",
            "ENTITY-MIB::entPhysicalTable",
            "ENTITY-SENSOR-MIB::entPhySensorTable",
            "CISCO-ENTITY-SENSOR-MIB::entSensorValueTable",
            "CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleTable",
            "BRIDGE-MIB::dot1dBasePortTable",
            "BRIDGE-MIB::dot1dStpPortTable",
            "BRIDGE-MIB::jaspyStpBridgeTable",
            "HP-ICF-RPVST-MIB::jaspyRpvstPortVlanRoleTable",
            "HP-ICF-RPVST-MIB::jaspyRpvstPortVlanStateTable",
            "HP-ICF-RPVST-MIB::jaspyRpvstPortVlanCostTable",
            "HP-ICF-RPVST-MIB::hpicfRpvstVlanTable",
            "CISCO-VTP-MIB::jaspyVlanTrunkPortTable",
            "CISCO-VTP-MIB::vtpVlanTable",
            "CISCO-VLAN-MEMBERSHIP-MIB::vmMembershipTable",
            "Q-BRIDGE-MIB::dot1qPortVlanTable",
            "Q-BRIDGE-MIB::dot1qVlanCurrentTable",
            "Q-BRIDGE-MIB::dot1qVlanStaticTable",
            "CISCO-PAGP-MIB::pagpPortTable",
            "IEEE8023-LAG-MIB::dot3adAggTable",
            "IEEE8023-LAG-MIB::dot3adAggPortTable",
            "POWER-ETHERNET-MIB::pethMainPseTable",
            "POWER-ETHERNET-MIB::pethPsePortTable",
            "CISCO-POWER-ETHERNET-EXT-MIB::cpeExtPsePortTable",
            "LLDP-MIB::lldpLocPortTable",
            "LLDP-MIB::lldpRemTable",
            "CISCO-CDP-MIB::cdpCacheTable",
            "CISCO-ERR-DISABLE-MIB::cErrDisableIfStatusTable",
            "CISCO-CLASS-BASED-QOS-MIB::cbQosServicePolicyTable",
            "CISCO-CLASS-BASED-QOS-MIB::cbQosObjectsTable",
            "CISCO-CLASS-BASED-QOS-MIB::cbQosPolicyMapCfgTable",
            "CISCO-CLASS-BASED-QOS-MIB::cbQosCMCfgTable",
            "CISCO-CLASS-BASED-QOS-MIB::cbQosCMStatsTable",
            "CISCO-CLASS-BASED-QOS-MIB::cbQosPoliceStatsTable",
        ];
        for id in tables {
            assert!(reg.table(id).is_some(), "table {} did not resolve", id);
        }
        let objects = [
            "SNMPv2-MIB::sysDescr",
            "BRIDGE-MIB::dot1dBaseBridgeAddress",
            "LLDP-MIB::lldpLocChassisId",
        ];
        for id in objects {
            assert!(reg.object(id).is_some(), "object {} did not resolve", id);
        }
    }

    #[test]
    fn real_ifxtable_index_via_augments() {
        let reg = match real_registry() {
            Some(r) => r,
            None => return,
        };
        let xtable = reg.table("IF-MIB::ifXTable").unwrap();
        assert_eq!(xtable.index.len(), 1);
        assert_eq!(xtable.index[0].id, "IF-MIB::ifIndex");
    }
}
