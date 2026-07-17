// Embedded SNMP client: replaces the snmpbot sidecar with an in-process snmp2
// (v2c) client. Tables are collected by walking each column independently with
// GETBULK and merging rows by their OID index suffix — deliberately unlike
// snmpbot's lockstep multi-column walk, which truncates on slow agents. The
// column walk / row merge / index decode / value render logic is written
// against a `Transport` trait so it is fully unit-testable with an in-memory
// fake; the real transport is a thin wrapper over snmp2::SyncSession.
use super::hostspec::HostSpec;
use super::mib::{MibObject, MibRegistry, MibTable};
use super::raw::RawValue;
use super::render::{decode_index, render_value};
use super::types::{SNMPBotObjectInstance, SNMPBotObjectResponse, SNMPBotResponse, SNMPBotResultEntry};
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

// Hard ceiling on rows per column, so a broken/looping agent can't spin
// forever. Far above any real table.
const MAX_COLUMN_ROWS: usize = 200_000;

// The wire operations the walk needs. Values are already owned (copied out of
// the snmp2 receive buffer) so results can outlive the next request.
pub trait Transport {
    // GETBULK of several OIDs at once (non-repeaters = 0). Returns the varbinds
    // in wire order: grouped by repetition, one varbind per requested OID in
    // request order per repetition (so varbind p belongs to requested OID
    // `p % oids.len()`). This is the lockstep primitive build_table walks on.
    fn getbulk_multi(&mut self, oids: &[&[u64]], max_repetitions: u32) -> Result<Vec<(Vec<u64>, RawValue)>, String>;
    // GET of an exact OID; returns the first varbind's value.
    fn get(&mut self, oid: &[u64]) -> Result<RawValue, String>;
    // GETNEXT of `oid`; returns the next (oid, value).
    fn getnext(&mut self, oid: &[u64]) -> Result<(Vec<u64>, RawValue), String>;
}

fn starts_with(oid: &[u64], prefix: &[u64]) -> bool {
    oid.len() >= prefix.len() && oid[..prefix.len()] == *prefix
}

// Assemble a full table response by walking every column in lockstep: one
// multi-varbind GETBULK per round advances all still-active columns at once,
// instead of one independent GETBULK sequence per column. This matches
// snmpbot's batched walk and cuts per-device round-trips by ~column-count
// (PERF.md #3). Robustness is preserved per column: a varbind outside the
// column prefix, an end-of-view sentinel, a non-increasing OID (looping-agent
// guard) or the MAX_COLUMN_ROWS cap ends that column's walk independently.
pub fn build_table<T: Transport>(
    t: &mut T,
    table: &MibTable,
    host_id: &str,
    max_repetitions: u32,
) -> Result<SNMPBotResponse, String> {
    // suffix -> { column-id -> value }
    let mut rows: BTreeMap<Vec<u64>, HashMap<String, RawValue>> = BTreeMap::new();

    let ncols = table.columns.len();
    // Per-column resume cursor: Some(last OID requested) while the column is
    // still being walked, None once it has ended. Seeded at the column base.
    let mut cursor: Vec<Option<Vec<u64>>> = table.columns.iter().map(|c| Some(c.oid.clone())).collect();
    let mut count: Vec<usize> = vec![0; ncols];

    loop {
        let active: Vec<usize> = (0..ncols).filter(|&i| cursor[i].is_some()).collect();
        if active.is_empty() {
            break;
        }
        // Owned copies so no borrow of `cursor` is held while we mutate it below.
        let request_owned: Vec<Vec<u64>> = active.iter().map(|&i| cursor[i].clone().unwrap()).collect();
        let request: Vec<&[u64]> = request_owned.iter().map(|v| v.as_slice()).collect();
        let varbinds = t.getbulk_multi(&request, max_repetitions)?;
        if varbinds.is_empty() {
            break;
        }

        let k = active.len();
        let mut progressed = false;
        for (p, (oid, value)) in varbinds.into_iter().enumerate() {
            let col = active[p % k];
            if cursor[col].is_none() {
                // Column already ended earlier in this same response.
                continue;
            }
            let base = &table.columns[col].oid;
            if value.is_end_of_view() || !starts_with(&oid, base) {
                cursor[col] = None; // walked past this column's prefix / end of MIB
                continue;
            }
            let advanced = oid.as_slice() > cursor[col].as_deref().unwrap();
            if !advanced {
                // Agent repeated or went backwards: stop this column to avoid a loop.
                cursor[col] = None;
                continue;
            }
            let suffix = oid[base.len()..].to_vec();
            rows.entry(suffix).or_default().insert(table.columns[col].id.clone(), value);
            count[col] += 1;
            cursor[col] = Some(oid);
            progressed = true;
            if count[col] >= MAX_COLUMN_ROWS {
                cursor[col] = None;
            }
        }
        if !progressed {
            break;
        }
    }

    let mut entries = Vec::new();
    for (suffix, column_values) in rows {
        let index = match decode_index(&suffix, &table.index) {
            Some(i) => i,
            None => continue, // undecodable index (non-integer / wrong arity)
        };
        let mut objects = HashMap::new();
        for column in &table.columns {
            if let Some(raw) = column_values.get(&column.id) {
                objects.insert(column.id.clone(), render_value(raw, &column.syntax));
            }
        }
        entries.push(SNMPBotResultEntry { host_i_d: host_id.to_string(), index, objects });
    }

    Ok(SNMPBotResponse {
        i_d: table.id.clone(),
        index_keys: table.index.iter().map(|o| o.id.clone()).collect(),
        object_keys: table.columns.iter().map(|o| o.id.clone()).collect(),
        entries,
    })
}

// Fetch a scalar (the `/objects/{id}` equivalent): GET the `.0` instance, and
// if that yields no value fall back to GETNEXT (agents with non-`.0`
// instances), accepting only a result still under the object prefix.
pub fn build_object<T: Transport>(t: &mut T, object: &MibObject, object_id: &str) -> Result<SNMPBotObjectResponse, String> {
    let mut instance_oid = object.oid.clone();
    instance_oid.push(0);

    let mut rendered = None;
    match t.get(&instance_oid) {
        Ok(value) if !value.is_end_of_view() && !matches!(value, RawValue::Null) => {
            rendered = Some(render_value(&value, &object.syntax));
        }
        Ok(_) => {}
        Err(e) => return Err(e),
    }
    if rendered.is_none() {
        if let Ok((oid, value)) = t.getnext(&object.oid) {
            if starts_with(&oid, &object.oid) && !value.is_end_of_view() {
                rendered = Some(render_value(&value, &object.syntax));
            }
        }
    }

    let instances = match rendered {
        Some(value) => vec![SNMPBotObjectInstance { value: Some(value) }],
        None => vec![],
    };
    Ok(SNMPBotObjectResponse { i_d: object_id.to_string(), instances })
}

// --- real snmp2-backed transport -----------------------------------------

struct SnmpSession {
    session: snmp2::SyncSession,
    retries: u32,
}

impl SnmpSession {
    fn open(fqdn: &str, community: &str, port: u16, timeout: Duration, retries: u32) -> Result<SnmpSession, String> {
        // An fqdn that already carries an explicit `:port` (or is a bracketed
        // IPv6 literal) is used as-is; otherwise the configured port is
        // appended. The explicit-port form is what the perf fleet uses to run
        // many simulated agents on one loopback IP.
        let destination = if fqdn.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()).is_some() && fqdn.contains(':') {
            fqdn.to_string()
        } else {
            format!("{}:{}", fqdn, port)
        };
        let session = snmp2::SyncSession::new_v2c(destination.as_str(), community.as_bytes(), Some(timeout), 0)
            .map_err(|e| format!("open session to {}: {}", destination, e))?;
        Ok(SnmpSession { session, retries })
    }
}

fn oid_to_vec(oid: &snmp2::Oid) -> Vec<u64> {
    oid.iter().map(|it| it.collect()).unwrap_or_default()
}

fn make_oid(components: &[u64]) -> Result<snmp2::Oid<'static>, String> {
    snmp2::Oid::from(components).map_err(|_| format!("invalid oid {:?}", components))
}

impl Transport for SnmpSession {
    fn getbulk_multi(&mut self, oids: &[&[u64]], max_repetitions: u32) -> Result<Vec<(Vec<u64>, RawValue)>, String> {
        // Build owned Oids once, then borrow them for the (possibly retried) request.
        let mut owned = Vec::with_capacity(oids.len());
        for o in oids {
            owned.push(make_oid(o)?);
        }
        let refs: Vec<&snmp2::Oid> = owned.iter().collect();
        let mut last_err = String::new();
        for _ in 0..=self.retries {
            match self.session.getbulk(&refs, 0, max_repetitions) {
                Ok(pdu) => {
                    let mut out = Vec::new();
                    for (o, v) in pdu.varbinds {
                        out.push((oid_to_vec(&o), super::raw::from_snmp2(&v)));
                    }
                    return Ok(out);
                }
                Err(e) => last_err = format!("getbulk: {:?}", e),
            }
        }
        Err(last_err)
    }

    fn get(&mut self, oid: &[u64]) -> Result<RawValue, String> {
        let target = make_oid(oid)?;
        let mut last_err = String::new();
        for _ in 0..=self.retries {
            match self.session.get(&target) {
                Ok(pdu) => {
                    return Ok(pdu.varbinds.map(|(_, v)| super::raw::from_snmp2(&v)).next().unwrap_or(RawValue::Null));
                }
                Err(e) => last_err = format!("get: {:?}", e),
            }
        }
        Err(last_err)
    }

    fn getnext(&mut self, oid: &[u64]) -> Result<(Vec<u64>, RawValue), String> {
        let target = make_oid(oid)?;
        let mut last_err = String::new();
        for _ in 0..=self.retries {
            match self.session.getnext(&target) {
                Ok(pdu) => {
                    return pdu
                        .varbinds
                        .map(|(o, v)| (oid_to_vec(&o), super::raw::from_snmp2(&v)))
                        .next()
                        .ok_or_else(|| "getnext: empty response".to_string());
                }
                Err(e) => last_err = format!("getnext: {:?}", e),
            }
        }
        Err(last_err)
    }
}

pub struct Embedded {
    mibs: Arc<MibRegistry>,
    port: u16,
    timeout: Duration,
    retries: u32,
    max_repetitions: u32,
}

impl Embedded {
    pub fn new(mibs: Arc<MibRegistry>, port: u16, timeout: Duration, retries: u32, max_repetitions: u32) -> Embedded {
        Embedded { mibs, port, timeout, retries, max_repetitions }
    }

    pub fn table(&self, host: &HostSpec, table_id: &str) -> Result<SNMPBotResponse, String> {
        let table = self.mibs.table(table_id).ok_or_else(|| format!("unknown table {}", table_id))?.clone();
        let community = host.effective_community().ok_or_else(|| "no community".to_string())?;
        let mut session = SnmpSession::open(&host.fqdn, &community, self.port, self.timeout, self.retries)?;
        build_table(&mut session, &table, &host.host_id(), self.max_repetitions)
    }

    pub fn object(&self, host: &HostSpec, object_id: &str) -> Result<SNMPBotObjectResponse, String> {
        let object = self.mibs.object(object_id).ok_or_else(|| format!("unknown object {}", object_id))?.clone();
        let community = host.effective_community().ok_or_else(|| "no community".to_string())?;
        let mut session = SnmpSession::open(&host.fqdn, &community, self.port, self.timeout, self.retries)?;
        build_object(&mut session, &object, object_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snmp::mib::Syntax;
    use crate::snmp::types::SNMPBotResultEntryObjectValue as Val;

    // In-memory SNMP agent over a sorted OID->value map.
    struct FakeAgent {
        tree: BTreeMap<Vec<u64>, RawValue>,
        fail: bool,
    }

    impl FakeAgent {
        fn new(entries: Vec<(Vec<u64>, RawValue)>) -> FakeAgent {
            FakeAgent { tree: entries.into_iter().collect(), fail: false }
        }
    }

    impl Transport for FakeAgent {
        // Mirror a standard agent's multi-OID GETBULK: for each requested OID
        // take its up-to-N lexical successors, then emit them grouped by
        // repetition (one varbind per requested OID per repetition), padding an
        // exhausted OID with endOfMibView so the wire alignment build_table
        // relies on is preserved.
        fn getbulk_multi(&mut self, oids: &[&[u64]], max_repetitions: u32) -> Result<Vec<(Vec<u64>, RawValue)>, String> {
            if self.fail {
                return Err("simulated timeout".to_string());
            }
            let per: Vec<Vec<(Vec<u64>, RawValue)>> = oids
                .iter()
                .map(|cur| {
                    self.tree
                        .range(cur.to_vec()..)
                        .filter(|(oid, _)| oid.as_slice() != *cur)
                        .take(max_repetitions as usize)
                        .map(|(oid, val)| (oid.clone(), val.clone()))
                        .collect()
                })
                .collect();
            let reps = per.iter().map(|p| p.len()).max().unwrap_or(0);
            let mut out = Vec::new();
            for rep in 0..reps {
                for (i, p) in per.iter().enumerate() {
                    if rep < p.len() {
                        out.push(p[rep].clone());
                    } else {
                        let last = p.last().map(|(o, _)| o.clone()).unwrap_or_else(|| oids[i].to_vec());
                        out.push((last, RawValue::EndOfMibView));
                    }
                }
            }
            Ok(out)
        }

        fn get(&mut self, oid: &[u64]) -> Result<RawValue, String> {
            if self.fail {
                return Err("simulated timeout".to_string());
            }
            Ok(self.tree.get(oid).cloned().unwrap_or(RawValue::NoSuchInstance))
        }

        fn getnext(&mut self, oid: &[u64]) -> Result<(Vec<u64>, RawValue), String> {
            if self.fail {
                return Err("simulated timeout".to_string());
            }
            self.tree
                .range(oid.to_vec()..)
                .find(|(o, _)| o.as_slice() != oid)
                .map(|(o, v)| (o.clone(), v.clone()))
                .ok_or_else(|| "end".to_string())
        }
    }

    fn obj(id: &str, oid: Vec<u64>, syntax: Syntax) -> Arc<MibObject> {
        Arc::new(MibObject { id: id.to_string(), oid, syntax })
    }

    // A tiny ifTable: index ifIndex, columns ifDescr (DisplayString) and
    // ifOperStatus (ENUM up/down), two rows.
    fn if_table() -> MibTable {
        let if_index = obj("IF-MIB::ifIndex", vec![1, 3, 6, 1, 2, 1, 2, 2, 1, 1], Syntax::Integer);
        let if_descr = obj("IF-MIB::ifDescr", vec![1, 3, 6, 1, 2, 1, 2, 2, 1, 2], Syntax::DisplayString);
        let mut status_names = HashMap::new();
        status_names.insert(1, "up".to_string());
        status_names.insert(2, "down".to_string());
        let if_status = obj("IF-MIB::ifOperStatus", vec![1, 3, 6, 1, 2, 1, 2, 2, 1, 8], Syntax::Enum(status_names));
        MibTable {
            id: "IF-MIB::ifTable".to_string(),
            oid: vec![1, 3, 6, 1, 2, 1, 2, 2],
            index: vec![if_index],
            columns: vec![if_descr, if_status],
        }
    }

    #[test]
    fn walk_merges_columns_by_index() {
        let table = if_table();
        let descr = &table.columns[0].oid;
        let status = &table.columns[1].oid;
        let mut d1 = descr.clone();
        d1.push(10101);
        let mut d2 = descr.clone();
        d2.push(10102);
        let mut s1 = status.clone();
        s1.push(10101);
        let mut s2 = status.clone();
        s2.push(10102);
        let mut agent = FakeAgent::new(vec![
            (d1, RawValue::OctetString(b"Gig0/1".to_vec())),
            (d2, RawValue::OctetString(b"Gig0/2".to_vec())),
            (s1, RawValue::Integer(1)),
            (s2, RawValue::Integer(2)),
        ]);

        let response = build_table(&mut agent, &table, "sw1.test.example", 10).unwrap();
        assert_eq!(response.i_d, "IF-MIB::ifTable");
        assert_eq!(response.entries.len(), 2);

        let by_index: HashMap<i64, &SNMPBotResultEntry> =
            response.entries.iter().map(|e| (e.index["IF-MIB::ifIndex"], e)).collect();
        let e1 = by_index[&10101];
        assert!(matches!(e1.objects.get("IF-MIB::ifDescr"), Some(Val::Str(s)) if s == "Gig0/1"));
        assert!(matches!(e1.objects.get("IF-MIB::ifOperStatus"), Some(Val::Str(s)) if s == "up"));
        let e2 = by_index[&10102];
        assert!(matches!(e2.objects.get("IF-MIB::ifOperStatus"), Some(Val::Str(s)) if s == "down"));
    }

    #[test]
    fn empty_table_yields_no_entries() {
        let table = if_table();
        let mut agent = FakeAgent::new(vec![]);
        let response = build_table(&mut agent, &table, "sw1.test.example", 10).unwrap();
        assert!(response.entries.is_empty());
    }

    #[test]
    fn transport_failure_propagates_as_err() {
        let table = if_table();
        let mut agent = FakeAgent::new(vec![]);
        agent.fail = true;
        assert!(build_table(&mut agent, &table, "sw1.test.example", 10).is_err());
    }

    #[test]
    fn columns_do_not_bleed_into_each_other() {
        // In the lockstep walk, a value belonging to ifOperStatus must not be
        // attributed to ifDescr (whose walk lexically runs into it).
        let table = if_table();
        let descr = &table.columns[0].oid;
        let status = &table.columns[1].oid;
        let mut d1 = descr.clone();
        d1.push(1);
        let mut s1 = status.clone();
        s1.push(1);
        let mut agent = FakeAgent::new(vec![
            (d1, RawValue::OctetString(b"a".to_vec())),
            (s1, RawValue::Integer(1)),
        ]);
        let response = build_table(&mut agent, &table, "sw1.test.example", 10).unwrap();
        assert_eq!(response.entries.len(), 1);
        let e = &response.entries[0];
        assert!(matches!(e.objects.get("IF-MIB::ifDescr"), Some(Val::Str(s)) if s == "a"));
        assert!(matches!(e.objects.get("IF-MIB::ifOperStatus"), Some(Val::Str(s)) if s == "up"));
    }

    #[test]
    fn lockstep_walk_spans_multiple_rounds() {
        // max_repetitions=1 forces one row per column per GETBULK, so a 3-row
        // table needs several rounds; every row must still be collected and
        // aligned to the right column.
        let table = if_table();
        let descr = &table.columns[0].oid;
        let status = &table.columns[1].oid;
        let mut entries = Vec::new();
        for i in 1..=3u64 {
            let mut d = descr.clone();
            d.push(i);
            let mut s = status.clone();
            s.push(i);
            entries.push((d, RawValue::OctetString(format!("if{}", i).into_bytes())));
            entries.push((s, RawValue::Integer(if i == 2 { 2 } else { 1 })));
        }
        let mut agent = FakeAgent::new(entries);
        let response = build_table(&mut agent, &table, "sw1.test.example", 1).unwrap();
        assert_eq!(response.entries.len(), 3);
        let by_index: HashMap<i64, &SNMPBotResultEntry> =
            response.entries.iter().map(|e| (e.index["IF-MIB::ifIndex"], e)).collect();
        assert!(matches!(by_index[&2].objects.get("IF-MIB::ifOperStatus"), Some(Val::Str(s)) if s == "down"));
        assert!(matches!(by_index[&3].objects.get("IF-MIB::ifDescr"), Some(Val::Str(s)) if s == "if3"));
    }

    #[test]
    fn scalar_get_dot_zero() {
        let sys_descr = obj("SNMPv2-MIB::sysDescr", vec![1, 3, 6, 1, 2, 1, 1, 1], Syntax::DisplayString);
        let mut instance = sys_descr.oid.clone();
        instance.push(0);
        let mut agent = FakeAgent::new(vec![(instance, RawValue::OctetString(b"Cisco IOS".to_vec()))]);
        let response = build_object(&mut agent, &sys_descr, "SNMPv2-MIB::sysDescr").unwrap();
        assert_eq!(response.instances.len(), 1);
        assert!(matches!(response.instances[0].value, Some(Val::Str(ref s)) if s == "Cisco IOS"));
    }

    #[test]
    fn scalar_getnext_fallback() {
        // No .0 instance; the value lives at .1 and GETNEXT should find it.
        let obj_def = obj("X::scalar", vec![1, 3, 6, 1, 4, 1, 9, 1], Syntax::Unsigned);
        let mut instance = obj_def.oid.clone();
        instance.push(1);
        let mut agent = FakeAgent::new(vec![(instance, RawValue::Counter32(42))]);
        let response = build_object(&mut agent, &obj_def, "X::scalar").unwrap();
        assert_eq!(response.instances.len(), 1);
        assert!(matches!(response.instances[0].value, Some(Val::Uint64(42))));
    }

    #[test]
    fn scalar_absent_yields_no_instances() {
        let obj_def = obj("X::scalar", vec![1, 3, 6, 1, 4, 1, 9, 1], Syntax::Unsigned);
        let mut agent = FakeAgent::new(vec![]);
        let response = build_object(&mut agent, &obj_def, "X::scalar").unwrap();
        assert!(response.instances.is_empty());
    }

    // Exercises the real snmp2 SyncSession wrapper end to end (new_v2c +
    // getbulk + error mapping) — the one path the fake transport can't cover.
    // Nothing listens on 127.0.0.1:161 in the test environment, so the request
    // times out and must surface as Err (not panic), which is exactly the
    // signal collectors treat as "keep last data".
    #[test]
    fn real_session_getbulk_times_out_cleanly() {
        let mut session = SnmpSession::open("127.0.0.1", "public", 161, Duration::from_millis(150), 0).unwrap();
        let oid = vec![1, 3, 6, 1, 2, 1, 2, 2, 1, 1];
        let result = session.getbulk_multi(&[oid.as_slice()], 5);
        assert!(result.is_err(), "getbulk to a dead port should time out, got {:?}", result);
    }
}
