// Embedded SNMP client: replaces the external snmpbot service with an in-process snmp2
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
use crate::utilities::perfstats::PERF;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

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

// --- adaptive per-device timeout ------------------------------------------
//
// High-CPU switches answer SNMP slower than a fixed timeout, so they time out
// every cycle (and, worse, their late replies poison a reused socket — see the
// session-reset logic below). Instead of one global timeout we learn a
// per-device timeout: grow it on timeouts up to a ceiling so a slow-but-alive
// switch's replies fit, keep it at the floor for fast devices, and collapse a
// genuinely-unresponsive device to a cheap fast-fail. The state is a pure,
// I/O-free function of the observed request outcomes (unit-tested below).

// Tuning knobs (from JASPY_SNMP_* config, main.rs). Copy-cheap so it can ride
// on each cached session and the per-device state handle.
#[derive(Clone, Copy, Debug)]
pub struct AdaptCfg {
    pub base_ms: u64,        // floor, and the fixed timeout when disabled
    pub max_ms: u64,         // ceiling
    pub alpha: f64,          // EWMA weight on the newest RTT
    pub factor: f64,         // target timeout = factor*ewma + margin
    pub margin_ms: u64,
    pub backoff: f64,        // multiplicative growth per timeout while ramping
    pub backoff_add_ms: u64, // additive growth per timeout while ramping
    pub dead_streak: u32,    // consecutive timeouts *at max* before fast-fail collapse
    pub enabled: bool,
}

impl AdaptCfg {
    // Non-adaptive: effective timeout is always base_ms (reproduces the old
    // fixed-timeout behaviour). Used as a default/rollback.
    pub fn fixed(base_ms: u64) -> AdaptCfg {
        AdaptCfg {
            base_ms,
            max_ms: base_ms,
            alpha: 0.3,
            factor: 2.0,
            margin_ms: 250,
            backoff: 1.5,
            backoff_add_ms: 500,
            dead_streak: 3,
            enabled: false,
        }
    }
}

// One request's outcome, as a latency signal for the adaptation step.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Sample {
    Ok(f64), // round-trip time in ms
    Timeout,
    Mismatch, // req-id desync — owned by the session-reset path, not a latency signal
    Error,    // genuine SNMP/MIB error — not a latency signal either
}

// Pure adaptation state for one device.
#[derive(Clone, Copy, Debug, PartialEq)]
struct DeviceState {
    ewma_ms: Option<f64>,   // None until the first successful RTT
    consec_timeouts: u32,   // consecutive timeouts (any), reset on success
    timeouts_at_max: u32,   // consecutive timeouts *while already at max*, reset on success
    effective_ms: u64,      // the timeout the next socket open should use
    dead: bool,             // collapsed: fast-fail (base timeout, no retries)
}

impl DeviceState {
    fn new(base_ms: u64) -> DeviceState {
        DeviceState { ewma_ms: None, consec_timeouts: 0, timeouts_at_max: 0, effective_ms: base_ms, dead: false }
    }
    fn effective_retries(&self, configured: u32) -> u32 {
        if self.dead { 0 } else { configured }
    }
}

// The pure adaptation step. EWMA moves only on success (a timeout is not a real
// RTT, only a lower bound). effective grows on each timeout up to max. A device
// is declared dead only after dead_streak consecutive timeouts *while already at
// max* — so a still-ramping slow switch is never mistaken for dead — and then
// pins to base + no retries until any success clears it.
fn adapt(s: &mut DeviceState, sample: Sample, c: &AdaptCfg) {
    if !c.enabled {
        *s = DeviceState::new(c.base_ms);
        return;
    }
    match sample {
        Sample::Mismatch | Sample::Error => {} // not a latency signal
        Sample::Ok(rtt) => {
            let e = match s.ewma_ms {
                Some(prev) => c.alpha * rtt + (1.0 - c.alpha) * prev,
                None => rtt,
            };
            s.ewma_ms = Some(e);
            s.consec_timeouts = 0;
            s.timeouts_at_max = 0;
            s.dead = false;
            let target = c.factor * e + c.margin_ms as f64;
            s.effective_ms = (target.round() as u64).clamp(c.base_ms, c.max_ms);
        }
        Sample::Timeout => {
            s.consec_timeouts = s.consec_timeouts.saturating_add(1);
            if s.dead {
                s.effective_ms = c.base_ms; // stay pinned until a success
                return;
            }
            if s.effective_ms >= c.max_ms {
                s.timeouts_at_max = s.timeouts_at_max.saturating_add(1);
                if s.timeouts_at_max >= c.dead_streak {
                    s.dead = true;
                    s.effective_ms = c.base_ms;
                }
            } else {
                let grown = s.effective_ms as f64 * c.backoff + c.backoff_add_ms as f64;
                s.effective_ms = (grown.round() as u64).clamp(c.base_ms, c.max_ms);
            }
        }
    }
}

// Shared per-device adaptation state. One per session_key, behind its own mutex
// so the hot record path never touches the registry lock.
struct DeviceLatency {
    state: Mutex<DeviceState>,
    cfg: AdaptCfg,
}

impl DeviceLatency {
    fn record(&self, sample: Sample) {
        if let Ok(mut s) = self.state.lock() {
            adapt(&mut s, sample, &self.cfg);
        }
    }
    // (effective timeout ms, effective retries) for the next session open.
    fn open_params(&self, configured_retries: u32) -> (u64, u32) {
        match self.state.lock() {
            Ok(s) => (s.effective_ms, s.effective_retries(configured_retries)),
            Err(_) => (self.cfg.base_ms, configured_retries),
        }
    }
    fn snapshot(&self) -> DeviceState {
        self.state.lock().map(|s| *s).unwrap_or_else(|_| DeviceState::new(self.cfg.base_ms))
    }
    // Overwrite from persisted values; a dead device stays collapsed until a
    // fresh success clears it.
    fn seed(&self, ewma_ms: Option<f64>, effective_ms: u64, consec_timeouts: u32, dead: bool) {
        if let Ok(mut s) = self.state.lock() {
            s.ewma_ms = ewma_ms;
            s.effective_ms = effective_ms.clamp(self.cfg.base_ms, self.cfg.max_ms);
            s.consec_timeouts = consec_timeouts;
            s.dead = dead;
            s.timeouts_at_max = if dead { self.cfg.dead_streak } else { 0 };
        }
    }
}

// Process-global registry of per-device adaptation state, keyed by session_key.
// Must be global (not thread-local): the poller threads that observe latency,
// the Rocket workers that read it for the device page, and the flush worker that
// persists it are all different threads, and the run_bounded worker pools drop
// their thread-locals every cycle.
static DEVICE_LATENCY: OnceLock<RwLock<HashMap<String, Arc<DeviceLatency>>>> = OnceLock::new();

fn latency_registry() -> &'static RwLock<HashMap<String, Arc<DeviceLatency>>> {
    DEVICE_LATENCY.get_or_init(|| RwLock::new(HashMap::new()))
}

// The adaptive config the embedded client was constructed with. Recorded once at
// Embedded::new so the debug endpoints can report the floor/ceiling/enabled even
// for a device that has no session in the registry yet.
static ADAPT_CFG: OnceLock<AdaptCfg> = OnceLock::new();

// Record the active adaptive config. Called from Embedded::new in production;
// mock mode calls it directly since it seeds the registry without an Embedded.
pub fn set_adapt_config(cfg: AdaptCfg) {
    let _ = ADAPT_CFG.set(cfg);
}

// The active adaptive config, if the embedded client has been constructed.
pub fn adapt_config() -> Option<AdaptCfg> {
    ADAPT_CFG.get().copied()
}

// Get-or-create the shared state handle for a session_key.
fn device_latency(key: &str, cfg: &AdaptCfg) -> Arc<DeviceLatency> {
    let reg = latency_registry();
    if let Ok(map) = reg.read() {
        if let Some(h) = map.get(key) {
            return h.clone();
        }
    }
    let mut map = reg.write().unwrap();
    map.entry(key.to_string())
        .or_insert_with(|| Arc::new(DeviceLatency { state: Mutex::new(DeviceState::new(cfg.base_ms)), cfg: *cfg }))
        .clone()
}

// The session_key for a device's primary polling session (the interface
// poller's: query-param style, no VLAN). Persistence and the device page key on
// this — a device's per-VLAN secondary sessions stay in-memory only.
pub fn primary_session_key(fqdn: &str, community: &str, port: u16) -> String {
    session_key(&HostSpec::with_community(fqdn, community), port)
}

// Per-device SNMP health for the device page. None => normal (no badge).
pub struct SnmpHealthSnapshot {
    pub status: &'static str, // "slow" | "dead"
    pub effective_timeout_ms: u64,
    pub ewma_latency_ms: Option<f64>,
}

pub fn snmp_health_for(fqdn: &str, community: &str) -> Option<SnmpHealthSnapshot> {
    // Match the device's primary polling session by (fqdn, community), ignoring
    // the port the route doesn't carry. The trailing separator keeps this from
    // matching per-VLAN secondary sessions, whose community is "community@vlan".
    let prefix = format!("{}\u{1f}{}\u{1f}", fqdn, community);
    let handle = {
        let map = latency_registry().read().ok()?;
        map.iter().find(|(k, _)| k.starts_with(&prefix)).map(|(_, v)| v.clone())?
    };
    let s = handle.snapshot();
    let status = if s.dead {
        "dead"
    } else if s.effective_ms > handle.cfg.base_ms {
        "slow"
    } else {
        return None;
    };
    Some(SnmpHealthSnapshot { status, effective_timeout_ms: s.effective_ms, ewma_latency_ms: s.ewma_ms })
}

// Durable projection of one device's state, for the periodic DB flush.
pub struct DeviceStateExport {
    pub ewma_ms: Option<f64>,
    pub effective_ms: u64,
    pub consec_timeouts: u32,
    pub dead: bool,
}

pub fn export_state(fqdn: &str, community: &str, port: u16) -> Option<DeviceStateExport> {
    let key = primary_session_key(fqdn, community, port);
    let handle = { latency_registry().read().ok()?.get(&key)?.clone() };
    let s = handle.snapshot();
    Some(DeviceStateExport { ewma_ms: s.ewma_ms, effective_ms: s.effective_ms, consec_timeouts: s.consec_timeouts, dead: s.dead })
}

// Seed a device's state from persistence at startup so a known-slow switch
// opens at its learned timeout immediately instead of re-ramping.
pub fn seed_state(fqdn: &str, community: &str, port: u16, cfg: &AdaptCfg, ewma_ms: Option<f64>, effective_ms: u64, consec_timeouts: u32, dead: bool) {
    let key = primary_session_key(fqdn, community, port);
    device_latency(&key, cfg).seed(ewma_ms, effective_ms, consec_timeouts, dead);
}

// Aggregate gauges for the perf endpoint (low cardinality, no per-device series).
// Returns (elevated device count, dead device count, max effective timeout ms).
pub fn adapt_aggregate() -> (u64, u64, u64) {
    let map = match latency_registry().read() {
        Ok(m) => m,
        Err(_) => return (0, 0, 0),
    };
    let mut elevated = 0u64;
    let mut dead = 0u64;
    let mut max_eff = 0u64;
    for handle in map.values() {
        let s = handle.snapshot();
        if s.dead {
            dead += 1;
        }
        if s.effective_ms > handle.cfg.base_ms {
            elevated += 1;
        }
        if s.effective_ms > max_eff {
            max_eff = s.effective_ms;
        }
    }
    (elevated, dead, max_eff)
}

// One live session's adaptive state, projected for the debug views. A session is
// the device's primary polling session (vlan == None) or a per-VLAN secondary
// session (vlan == Some(n)), distinguished by the "community@vlan" community
// segment of its registry key.
pub struct SessionSnapshot {
    pub vlan: Option<u32>,
    pub port: u16,
    pub effective_ms: u64,
    pub base_ms: u64,
    pub ewma_ms: Option<f64>,
    pub consec_timeouts: u32,
    pub dead: bool,
    pub status: &'static str, // "normal" | "slow" | "dead"
}

// Project a registry entry into a SessionSnapshot. Returns None if the key does
// not split into the expected fqdn/community/port form. Also yields the fqdn so
// callers enumerating the whole registry can group by device.
fn session_snapshot(key: &str, handle: &DeviceLatency) -> Option<(String, SessionSnapshot)> {
    let mut parts = key.split('\u{1f}');
    let fqdn = parts.next()?.to_string();
    let community = parts.next()?;
    let port: u16 = parts.next()?.parse().ok()?;
    // Per-VLAN secondary sessions carry a "community@vlan" community.
    let vlan = community.rsplit_once('@').and_then(|(_, v)| v.parse::<u32>().ok());
    let s = handle.snapshot();
    let status = if s.dead {
        "dead"
    } else if s.effective_ms > handle.cfg.base_ms {
        "slow"
    } else {
        "normal"
    };
    Some((
        fqdn,
        SessionSnapshot {
            vlan,
            port,
            effective_ms: s.effective_ms,
            base_ms: handle.cfg.base_ms,
            ewma_ms: s.ewma_ms,
            consec_timeouts: s.consec_timeouts,
            dead: s.dead,
            status,
        },
    ))
}

// Every live adaptive session for one device (primary + per-VLAN), for the
// device-page Debugging section. Empty in snmpbot mode or for an unpolled device.
// Sorted primary first, then by ascending VLAN.
pub fn sessions_for(fqdn: &str) -> Vec<SessionSnapshot> {
    let prefix = format!("{}\u{1f}", fqdn);
    let map = match latency_registry().read() {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<SessionSnapshot> = map
        .iter()
        .filter(|(k, _)| k.starts_with(&prefix))
        .filter_map(|(k, h)| session_snapshot(k, h).map(|(_, snap)| snap))
        .collect();
    out.sort_by_key(|s| s.vlan.map(|v| v as u64 + 1).unwrap_or(0));
    out
}

// The entire registry (all devices, primary + per-VLAN), for the fleet-wide
// /dev/metrics/snmp-adaptive dump. Each entry carries its device fqdn.
pub fn all_adaptive_sessions() -> Vec<(String, SessionSnapshot)> {
    let map = match latency_registry().read() {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<(String, SessionSnapshot)> =
        map.iter().filter_map(|(k, h)| session_snapshot(k, h)).collect();
    out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.vlan.cmp(&b.1.vlan)));
    out
}

// --- real snmp2-backed transport -----------------------------------------

struct SnmpSession {
    session: snmp2::SyncSession,
    retries: u32,
    // Shared adaptation state for this device; the transport records each
    // request's outcome here.
    latency: Arc<DeviceLatency>,
    // The timeout this socket was actually opened with, so with_session can tell
    // when the effective timeout has grown past it and a reopen is due.
    opened_timeout_ms: u64,
}

impl SnmpSession {
    fn open(fqdn: &str, community: &str, port: u16, timeout: Duration, retries: u32, latency: Arc<DeviceLatency>) -> Result<SnmpSession, String> {
        PERF.record_session_open();
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
        Ok(SnmpSession { session, retries, latency, opened_timeout_ms: timeout.as_millis() as u64 })
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
        // Record exactly one latency sample per logical query (the terminal
        // outcome of the retry loop), so a device's adaptation counters count
        // logical queries, not individual UDP retries.
        let mut last_err = String::new();
        let mut outcome = Sample::Error;
        let mut ok: Option<Vec<(Vec<u64>, RawValue)>> = None;
        for _ in 0..=self.retries {
            let t = Instant::now();
            match self.session.getbulk(&refs, 0, max_repetitions) {
                Ok(pdu) => {
                    outcome = Sample::Ok(t.elapsed().as_secs_f64() * 1000.0);
                    let mut out = Vec::new();
                    for (o, v) in pdu.varbinds {
                        out.push((oid_to_vec(&o), super::raw::from_snmp2(&v)));
                    }
                    ok = Some(out);
                    break;
                }
                // A req-id mismatch means the socket read a stale/late datagram:
                // don't retry on this poisoned socket — bail so with_session
                // reopens a clean one and retries.
                Err(snmp2::Error::RequestIdMismatch) => {
                    outcome = Sample::Mismatch;
                    last_err = "getbulk: RequestIdMismatch".to_string();
                    break;
                }
                Err(snmp2::Error::Receive) => {
                    outcome = Sample::Timeout;
                    last_err = "getbulk: Receive".to_string();
                }
                Err(e) => {
                    outcome = Sample::Error;
                    last_err = format!("getbulk: {:?}", e);
                }
            }
        }
        self.latency.record(outcome);
        match ok {
            Some(out) => Ok(out),
            None => Err(last_err),
        }
    }

    fn get(&mut self, oid: &[u64]) -> Result<RawValue, String> {
        let target = make_oid(oid)?;
        let mut last_err = String::new();
        let mut outcome = Sample::Error;
        let mut ok: Option<RawValue> = None;
        for _ in 0..=self.retries {
            let t = Instant::now();
            match self.session.get(&target) {
                Ok(pdu) => {
                    outcome = Sample::Ok(t.elapsed().as_secs_f64() * 1000.0);
                    ok = Some(pdu.varbinds.map(|(_, v)| super::raw::from_snmp2(&v)).next().unwrap_or(RawValue::Null));
                    break;
                }
                Err(snmp2::Error::RequestIdMismatch) => {
                    outcome = Sample::Mismatch;
                    last_err = "get: RequestIdMismatch".to_string();
                    break;
                }
                Err(snmp2::Error::Receive) => {
                    outcome = Sample::Timeout;
                    last_err = "get: Receive".to_string();
                }
                Err(e) => {
                    outcome = Sample::Error;
                    last_err = format!("get: {:?}", e);
                }
            }
        }
        self.latency.record(outcome);
        match ok {
            Some(v) => Ok(v),
            None => Err(last_err),
        }
    }

    fn getnext(&mut self, oid: &[u64]) -> Result<(Vec<u64>, RawValue), String> {
        let target = make_oid(oid)?;
        let mut last_err = String::new();
        let mut outcome = Sample::Error;
        let mut ok: Option<(Vec<u64>, RawValue)> = None;
        for _ in 0..=self.retries {
            let t = Instant::now();
            match self.session.getnext(&target) {
                Ok(pdu) => {
                    outcome = Sample::Ok(t.elapsed().as_secs_f64() * 1000.0);
                    match pdu.varbinds.map(|(o, v)| (oid_to_vec(&o), super::raw::from_snmp2(&v))).next() {
                        Some(vb) => {
                            ok = Some(vb);
                            break;
                        }
                        None => {
                            last_err = "getnext: empty response".to_string();
                            break;
                        }
                    }
                }
                Err(snmp2::Error::RequestIdMismatch) => {
                    outcome = Sample::Mismatch;
                    last_err = "getnext: RequestIdMismatch".to_string();
                    break;
                }
                Err(snmp2::Error::Receive) => {
                    outcome = Sample::Timeout;
                    last_err = "getnext: Receive".to_string();
                }
                Err(e) => {
                    outcome = Sample::Error;
                    last_err = format!("getnext: {:?}", e);
                }
            }
        }
        self.latency.record(outcome);
        match ok {
            Some(vb) => Ok(vb),
            None => Err(last_err),
        }
    }
}

// Per-thread SNMP session cache, keyed by destination (PERF.md #6). snmp2
// sessions are single-owner (one request/reply socket), so a per-thread cache
// is the natural unit: the thread-per-device interface poller reuses one
// session across a device's tables and across every cycle, and a run_bounded
// worker reuses a session across a device's tables while it holds that device.
// A cached socket is dropped on ANY request error (see run_on_cached_session):
// a UDP socket that just timed out may have a stale/late reply buffered, and
// reusing it reads that stale datagram on the next request (req-id mismatch, or
// worse, a silent wrong-table read). Reopening gets a fresh source port so the
// orphaned datagram is unroutable.
thread_local! {
    static SESSIONS: RefCell<HashMap<String, SnmpSession>> = RefCell::new(HashMap::new());
}

// Destination identity for the session cache. Distinct communities (which fold
// in per-VLAN community indexing) and ports get distinct sessions.
fn session_key(host: &HostSpec, port: u16) -> String {
    format!("{}\u{1f}{}\u{1f}{}", host.fqdn, host.effective_community().unwrap_or_default(), port)
}

// Does this error mean the cached socket is desynced (holding a stale datagram),
// so reopening + retrying on a clean socket is worth it this cycle? Only a
// req-id mismatch qualifies. A plain timeout ("Receive") evicts too (the socket
// may still receive a late reply) but is not retried — retrying a dead/slow
// device would just double the wait. snmp2 errors are formatted with {:?}, so
// the Debug name appears verbatim.
fn session_desynced(err: &str) -> bool {
    err.contains("RequestIdMismatch")
}

// Ensure a session for `key` exists, run `f` on it, and reset the cache entry on
// error so a poisoned socket can't persist across calls:
//   * ok                  -> session stays cached (the reuse win)
//   * timeout / genuine    -> evict only; next call reopens lazily (no retry)
//   * desync (req-id)      -> evict, reopen a clean socket, retry `f` once; keep
//                             the fresh session on success, else drop it
// Socket-free and generic so the eviction/reopen policy is unit-testable without
// real sockets. Hard bound: at most two opens per call. `open`/`f` may each run
// twice (hence `Fn`); the borrow of the map is held across them, which is safe
// because neither re-enters the cache.
fn run_on_cached_session<S, R>(
    cache: &mut HashMap<String, S>,
    key: &str,
    open: impl Fn() -> Result<S, String>,
    f: impl Fn(&mut S) -> Result<R, String>,
) -> Result<R, String> {
    if !cache.contains_key(key) {
        cache.insert(key.to_string(), open()?);
    }
    let err = match f(cache.get_mut(key).unwrap()) {
        Ok(v) => return Ok(v),
        Err(e) => e,
    };
    cache.remove(key); // drop the (possibly poisoned) session; its socket closes
    if !session_desynced(&err) {
        return Err(err);
    }
    // Poisoned socket: reopen clean and retry once so this cycle still gets data.
    let mut fresh = open()?;
    match f(&mut fresh) {
        Ok(v) => {
            cache.insert(key.to_string(), fresh);
            Ok(v)
        }
        Err(e) => Err(e),
    }
}

pub struct Embedded {
    mibs: Arc<MibRegistry>,
    port: u16,
    retries: u32,
    max_repetitions: u32,
    adapt: AdaptCfg,
}

impl Embedded {
    pub fn new(mibs: Arc<MibRegistry>, port: u16, retries: u32, max_repetitions: u32, adapt: AdaptCfg) -> Embedded {
        set_adapt_config(adapt);
        Embedded { mibs, port, retries, max_repetitions, adapt }
    }

    // Run `f` with a cached (or freshly opened) session for this destination,
    // resetting the session on error (run_on_cached_session) and reopening when
    // the device's adaptive timeout has grown past the cached socket's baked
    // timeout. Never reopens to shrink — a larger socket timeout is harmless on
    // a now-fast device and shrinking would thrash.
    fn with_session<T>(
        &self,
        host: &HostSpec,
        community: &str,
        f: impl Fn(&mut SnmpSession) -> Result<T, String>,
    ) -> Result<T, String> {
        let key = session_key(host, self.port);
        let latency = device_latency(&key, &self.adapt);
        let (eff_timeout, eff_retries) = latency.open_params(self.retries);
        let fqdn = &host.fqdn;
        let port = self.port;
        SESSIONS.with(|cache| {
            let mut cache = cache.borrow_mut();
            // Reopen-on-grow: drop a cached socket whose baked timeout is now too
            // small so the reopen below picks up the larger effective timeout.
            if cache.get(&key).is_some_and(|s| s.opened_timeout_ms < eff_timeout) {
                cache.remove(&key);
            }
            run_on_cached_session(
                &mut cache,
                &key,
                || SnmpSession::open(fqdn, community, port, Duration::from_millis(eff_timeout), eff_retries, latency.clone()),
                &f,
            )
        })
    }

    pub fn table(&self, host: &HostSpec, table_id: &str) -> Result<SNMPBotResponse, String> {
        let table = self.mibs.table(table_id).ok_or_else(|| format!("unknown table {}", table_id))?.clone();
        let community = host.effective_community().ok_or_else(|| "no community".to_string())?;
        let host_id = host.host_id();
        let max_rep = self.max_repetitions;
        self.with_session(host, &community, |session| build_table(session, &table, &host_id, max_rep))
    }

    pub fn object(&self, host: &HostSpec, object_id: &str) -> Result<SNMPBotObjectResponse, String> {
        let object = self.mibs.object(object_id).ok_or_else(|| format!("unknown object {}", object_id))?.clone();
        let community = host.effective_community().ok_or_else(|| "no community".to_string())?;
        self.with_session(host, &community, |session| build_object(session, &object, object_id))
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
    fn session_key_separates_by_community_and_port() {
        let a = HostSpec::with_community("sw1.example.com", "public");
        let b = HostSpec::with_community("sw1.example.com", "private");
        // Same host, different community -> different session.
        assert_ne!(session_key(&a, 161), session_key(&b, 161));
        // Same host+community, different port -> different session.
        assert_ne!(session_key(&a, 161), session_key(&a, 16100));
        // Per-VLAN community indexing folds into the key (distinct VLAN sessions).
        let v100 = HostSpec::parse("public@100@sw1.example.com");
        let v200 = HostSpec::parse("public@200@sw1.example.com");
        assert_ne!(session_key(&v100, 161), session_key(&v200, 161));
        // Identical destination -> identical key (reused).
        assert_eq!(session_key(&a, 161), session_key(&HostSpec::with_community("sw1.example.com", "public"), 161));
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
        let mut session = SnmpSession::open("127.0.0.1", "public", 161, Duration::from_millis(150), 0, test_latency()).unwrap();
        let oid = vec![1, 3, 6, 1, 2, 1, 2, 2, 1, 1];
        let result = session.getbulk_multi(&[oid.as_slice()], 5);
        assert!(result.is_err(), "getbulk to a dead port should time out, got {:?}", result);
    }

    // --- Part A: session-reset policy -------------------------------------

    fn test_latency() -> Arc<DeviceLatency> {
        Arc::new(DeviceLatency { state: Mutex::new(DeviceState::new(2000)), cfg: AdaptCfg::fixed(2000) })
    }

    #[test]
    fn desync_classifier_matches_reqid_mismatch_only() {
        assert!(session_desynced("getbulk: RequestIdMismatch"));
        assert!(!session_desynced("getbulk: Receive")); // plain timeout -> evict only
        assert!(!session_desynced("get: NoSuchInstance"));
        assert!(!session_desynced(""));
    }

    #[test]
    fn healthy_call_opens_once_and_reuses() {
        let mut cache: HashMap<String, u32> = HashMap::new();
        let opens = std::cell::Cell::new(0u32);
        let open = || {
            opens.set(opens.get() + 1);
            Ok::<u32, String>(opens.get())
        };
        let f = |_s: &mut u32| Ok::<u32, String>(7);
        assert_eq!(run_on_cached_session(&mut cache, "k", &open, &f), Ok(7));
        assert_eq!(run_on_cached_session(&mut cache, "k", &open, &f), Ok(7));
        assert_eq!(opens.get(), 1); // reused, not reopened
        assert!(cache.contains_key("k"));
    }

    #[test]
    fn timeout_evicts_without_retry() {
        let mut cache: HashMap<String, u32> = HashMap::new();
        let opens = std::cell::Cell::new(0u32);
        let calls = std::cell::Cell::new(0u32);
        let open = || {
            opens.set(opens.get() + 1);
            Ok::<u32, String>(1)
        };
        let f = |_s: &mut u32| {
            calls.set(calls.get() + 1);
            Err::<u32, String>("getbulk: Receive".into())
        };
        assert!(run_on_cached_session(&mut cache, "k", &open, &f).is_err());
        assert_eq!(opens.get(), 1); // no reopen on a plain timeout
        assert_eq!(calls.get(), 1); // no retry
        assert!(!cache.contains_key("k")); // evicted
    }

    #[test]
    fn desync_reopens_and_retries_once() {
        let mut cache: HashMap<String, u32> = HashMap::new();
        let opens = std::cell::Cell::new(0u32);
        let calls = std::cell::Cell::new(0u32);
        let open = || {
            opens.set(opens.get() + 1);
            Ok::<u32, String>(opens.get())
        };
        let f = |_s: &mut u32| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                Err::<u32, String>("getbulk: RequestIdMismatch".into())
            } else {
                Ok(42)
            }
        };
        assert_eq!(run_on_cached_session(&mut cache, "k", &open, &f), Ok(42));
        assert_eq!(opens.get(), 2); // reopened once on the mismatch
        assert_eq!(calls.get(), 2); // retried once, succeeded
        assert!(cache.contains_key("k")); // fresh session cached
    }

    #[test]
    fn desync_retry_failure_leaves_no_cached_session() {
        let mut cache: HashMap<String, u32> = HashMap::new();
        let opens = std::cell::Cell::new(0u32);
        let open = || {
            opens.set(opens.get() + 1);
            Ok::<u32, String>(1)
        };
        let f = |_s: &mut u32| Err::<u32, String>("getbulk: RequestIdMismatch".into());
        assert!(run_on_cached_session(&mut cache, "k", &open, &f).is_err());
        assert_eq!(opens.get(), 2); // bounded: at most one reopen
        assert!(!cache.contains_key("k"));
    }

    // --- Part B: adaptive timeout ----------------------------------------

    fn adapt_cfg() -> AdaptCfg {
        AdaptCfg {
            base_ms: 2000,
            max_ms: 10000,
            alpha: 0.3,
            factor: 2.0,
            margin_ms: 250,
            backoff: 1.5,
            backoff_add_ms: 500,
            dead_streak: 3,
            enabled: true,
        }
    }

    #[test]
    fn adapt_fast_device_stays_at_floor() {
        let c = adapt_cfg();
        let mut s = DeviceState::new(c.base_ms);
        for _ in 0..5 {
            adapt(&mut s, Sample::Ok(80.0), &c);
        }
        assert_eq!(s.effective_ms, c.base_ms);
        assert!(!s.dead);
    }

    #[test]
    fn adapt_slow_device_grows_then_settles() {
        let c = adapt_cfg();
        let mut s = DeviceState::new(c.base_ms);
        adapt(&mut s, Sample::Timeout, &c);
        assert!(s.effective_ms > c.base_ms && s.effective_ms <= c.max_ms);
        adapt(&mut s, Sample::Ok(1800.0), &c);
        assert_eq!(s.consec_timeouts, 0);
        assert!(!s.dead);
        assert_eq!(s.effective_ms, 3850); // factor*ewma + margin = 2*1800 + 250
    }

    #[test]
    fn adapt_dead_device_collapses_and_fastfails() {
        let c = adapt_cfg();
        let mut s = DeviceState::new(c.base_ms);
        for _ in 0..20 {
            adapt(&mut s, Sample::Timeout, &c);
            if s.dead {
                break;
            }
        }
        assert!(s.dead);
        assert_eq!(s.effective_ms, c.base_ms);
        assert_eq!(s.effective_retries(2), 0); // fast-fail
        adapt(&mut s, Sample::Ok(50.0), &c); // any success clears it
        assert!(!s.dead);
        assert_eq!(s.effective_retries(2), 2);
    }

    #[test]
    fn adapt_growth_clamped_to_max() {
        let c = adapt_cfg();
        let mut s = DeviceState::new(c.base_ms);
        for _ in 0..50 {
            adapt(&mut s, Sample::Timeout, &c);
        }
        assert!(s.effective_ms <= c.max_ms);
    }

    #[test]
    fn adapt_mismatch_and_error_are_noops() {
        let c = adapt_cfg();
        let mut s = DeviceState::new(c.base_ms);
        adapt(&mut s, Sample::Ok(1000.0), &c);
        let before = s;
        adapt(&mut s, Sample::Mismatch, &c);
        adapt(&mut s, Sample::Error, &c);
        assert_eq!(s, before);
    }

    #[test]
    fn adapt_disabled_pins_to_base() {
        let mut c = adapt_cfg();
        c.enabled = false;
        let mut s = DeviceState::new(c.base_ms);
        adapt(&mut s, Sample::Timeout, &c);
        assert_eq!(s.effective_ms, c.base_ms);
        adapt(&mut s, Sample::Ok(9000.0), &c);
        assert_eq!(s.effective_ms, c.base_ms);
    }

    // --- Part C: session enumeration for the debug views -------------------
    // These seed the process-global registry; each uses a unique fqdn so the
    // fqdn-prefixed reads stay isolated under parallel test execution.

    #[test]
    fn sessions_for_lists_primary_and_vlan_sorted() {
        let c = adapt_cfg();
        let fqdn = "sw-sessions-test.example.net";
        // Primary (fast), and two per-VLAN sessions out of numeric order.
        seed_state(fqdn, "public", 161, &c, Some(12.0), 2000, 0, false);
        seed_state(fqdn, "public@200", 161, &c, None, 2000, 6, true);
        seed_state(fqdn, "public@100", 161, &c, Some(2850.0), 6000, 0, false);

        let sessions = sessions_for(fqdn);
        assert_eq!(sessions.len(), 3);
        // Primary first, then ascending VLAN.
        assert_eq!(sessions[0].vlan, None);
        assert_eq!(sessions[0].status, "normal");
        assert_eq!(sessions[0].base_ms, c.base_ms);
        assert_eq!(sessions[1].vlan, Some(100));
        assert_eq!(sessions[1].effective_ms, 6000);
        assert_eq!(sessions[1].status, "slow");
        assert_eq!(sessions[2].vlan, Some(200));
        assert!(sessions[2].dead);
        assert_eq!(sessions[2].status, "dead");
    }

    #[test]
    fn sessions_for_unknown_device_is_empty() {
        assert!(sessions_for("no-such-device-xyz.example.net").is_empty());
    }

    #[test]
    fn all_adaptive_sessions_includes_seeded_device() {
        let c = adapt_cfg();
        let fqdn = "sw-fleet-test.example.net";
        seed_state(fqdn, "public", 161, &c, Some(30.0), 2000, 0, false);
        seed_state(fqdn, "public@42", 161, &c, Some(4000.0), 8500, 0, false);

        let mine: Vec<_> = all_adaptive_sessions().into_iter().filter(|(f, _)| f == fqdn).collect();
        assert_eq!(mine.len(), 2);
        assert!(mine.iter().any(|(_, s)| s.vlan == Some(42) && s.effective_ms == 8500 && s.status == "slow"));
    }

    #[test]
    fn adapt_config_reports_after_set() {
        // OnceLock: some other test in this process may set it first, so assert
        // presence and shape rather than exact equality with our cfg.
        set_adapt_config(adapt_cfg());
        let cfg = adapt_config().expect("config set");
        assert!(cfg.max_ms >= cfg.base_ms);
    }
}
