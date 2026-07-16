// snmpbot-compatible response models. Historically these lived in
// collectors/poller.rs (the first collector ported from the standalone
// jaspy-poller); they moved here when the SNMP access layer was abstracted
// behind `SnmpSource` so both the snmpbot HTTP client and the embedded snmp2
// client can produce the exact same shapes the collectors deserialize.
// collectors/poller.rs re-exports every type below, so existing
// `crate::collectors::poller::SNMP*` references keep working unchanged.
use std::collections::HashMap;

// snmpbot is Go and marshals nil slices/maps as JSON null (e.g.
// "Entries": null when a table walk returns no rows — normal for a switch
// without the MIB in question). Treat null as empty instead of failing the
// whole decode.
pub fn null_to_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + serde::Deserialize<'de>,
{
    use serde::Deserialize;
    let opt = Option::<T>::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum SNMPBotResultEntryObjectValue {
    Uint64(u64),
    Float64(f64),
    Str(String),
    Bool(bool),
    Empty,
    // Anything else snmpbot may emit (e.g. per-object error structs); callers
    // treat it as an absent value rather than failing the whole table.
    Other(serde_json::Value),
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SNMPBotResultEntry {
    pub host_i_d: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub index: HashMap<String, i64>,
    #[serde(default, deserialize_with = "null_to_default")]
    pub objects: HashMap<String, SNMPBotResultEntryObjectValue>,
}

// Single-object query response (`GET /api/hosts/{host}/objects/{id}`), shared
// by the discovery engine and the entitypoller's bridge-scalar polling.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SNMPBotObjectInstance {
    #[serde(default)]
    pub value: Option<SNMPBotResultEntryObjectValue>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SNMPBotObjectResponse {
    #[allow(dead_code)]
    pub i_d: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub instances: Vec<SNMPBotObjectInstance>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SNMPBotResponse {
    pub i_d: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub index_keys: Vec<String>,
    #[serde(default, deserialize_with = "null_to_default")]
    pub object_keys: Vec<String>,
    #[serde(default, deserialize_with = "null_to_default")]
    pub entries: Vec<SNMPBotResultEntry>,
}
