// SNMP v2c fleet simulator for the nexus perf suite.
//
// Binds one UDP socket per simulated switch (distinct loopback IP, one shared
// non-privileged port) and answers GET / GETNEXT / GETBULK for a synthetic
// IF-MIB (ifTable + ifXTable, configurable interface count). Counter columns
// increase monotonically with wall time so nexus's counter validation and
// health deltas behave like a real device.
//
// Requests are parsed with snmp2 (the same library nexus's embedded client
// uses), so the request wire format is exactly what nexus emits. Responses are
// BER-encoded here by hand (snmp2's encoder is not public); the encoder is
// round-trip-tested against snmp2's own parser in the tests below.
//
// Addressing: nexus is pointed at devices whose fqdn is a loopback IP literal
// (127.0.0.2 .. 127.0.0.N+1) and JASPY_SNMP_PORT is set to our port. On Linux
// the whole 127.0.0.0/8 is loopback with no setup; on macOS run
// perf/setup-loopback-macos.sh first to alias the extra addresses.
use std::collections::BTreeMap;
use std::net::UdpSocket;
use std::sync::Arc;
use std::time::Instant;

// ---- IF-MIB layout ---------------------------------------------------------

const IF_TABLE: &[u64] = &[1, 3, 6, 1, 2, 1, 2, 2, 1];
const IFX_TABLE: &[u64] = &[1, 3, 6, 1, 2, 1, 31, 1, 1, 1];

// How each column's value is produced at response time.
#[derive(Clone)]
enum Gen {
    // Monotonic 64-bit counter: base + rate_per_sec * elapsed_seconds.
    Counter64 { base: u64, rate: u64 },
    // Monotonic 32-bit counter (wraps naturally at u32).
    Counter32 { base: u32, rate: u32 },
    // Fixed gauge (e.g. ifHighSpeed = 1000 Mbps).
    Gauge32(u32),
    // Fixed signed integer (e.g. ifOperStatus = 1 "up").
    Integer(i64),
    // Fixed display string (ifDescr / ifName).
    OctetString(Vec<u8>),
}

impl Gen {
    fn value(&self, elapsed_s: u64) -> Value {
        match self {
            Gen::Counter64 { base, rate } => Value::Counter64(base.wrapping_add(rate.wrapping_mul(elapsed_s))),
            Gen::Counter32 { base, rate } => Value::Counter32(base.wrapping_add(rate.wrapping_mul(elapsed_s as u32))),
            Gen::Gauge32(v) => Value::Gauge32(*v),
            Gen::Integer(v) => Value::Integer(*v),
            Gen::OctetString(b) => Value::OctetString(b.clone()),
        }
    }
}

#[derive(Clone)]
enum Value {
    Counter64(u64),
    Counter32(u32),
    Gauge32(u32),
    Integer(i64),
    OctetString(Vec<u8>),
    EndOfMibView,
}

// The static OID tree for a switch with `interfaces` ports. Keys are full OIDs
// (column base + ifIndex); values are generators evaluated per response. All
// switches share one tree — they differ only by source IP.
struct Mib {
    tree: BTreeMap<Vec<u64>, Gen>,
}

impl Mib {
    fn build(interfaces: u32) -> Mib {
        let mut tree = BTreeMap::new();
        let col = |table: &[u64], col: u64, gen_for: &dyn Fn(u32) -> Gen, tree: &mut BTreeMap<Vec<u64>, Gen>| {
            for i in 1..=interfaces {
                let mut oid = table.to_vec();
                oid.push(col);
                oid.push(i as u64);
                tree.insert(oid, gen_for(i));
            }
        };

        // ifTable columns the poller reads.
        col(IF_TABLE, 2, &|i| Gen::OctetString(format!("GigabitEthernet0/{}", i).into_bytes()), &mut tree); // ifDescr
        col(IF_TABLE, 8, &|_| Gen::Integer(1), &mut tree);                                                  // ifOperStatus = up
        col(IF_TABLE, 14, &|_| Gen::Counter32 { base: 0, rate: 1 }, &mut tree);                             // ifInErrors
        col(IF_TABLE, 19, &|_| Gen::Counter32 { base: 0, rate: 1 }, &mut tree);                             // ifOutDiscards
        col(IF_TABLE, 20, &|_| Gen::Counter32 { base: 0, rate: 1 }, &mut tree);                             // ifOutErrors

        // ifXTable columns the poller reads (HC counters + name + speed).
        col(IFX_TABLE, 1, &|i| Gen::OctetString(format!("Gi0/{}", i).into_bytes()), &mut tree);             // ifName
        col(IFX_TABLE, 6, &|_| Gen::Counter64 { base: 1_000_000, rate: 12_500_000 }, &mut tree);            // ifHCInOctets (~100 Mbit/s)
        col(IFX_TABLE, 7, &|_| Gen::Counter64 { base: 1000, rate: 10_000 }, &mut tree);                     // ifHCInUcastPkts
        col(IFX_TABLE, 8, &|_| Gen::Counter64 { base: 100, rate: 50 }, &mut tree);                          // ifHCInMulticastPkts
        col(IFX_TABLE, 9, &|_| Gen::Counter64 { base: 100, rate: 20 }, &mut tree);                          // ifHCInBroadcastPkts
        col(IFX_TABLE, 10, &|_| Gen::Counter64 { base: 2_000_000, rate: 12_500_000 }, &mut tree);           // ifHCOutOctets
        col(IFX_TABLE, 11, &|_| Gen::Counter64 { base: 1000, rate: 10_000 }, &mut tree);                    // ifHCOutUcastPkts
        col(IFX_TABLE, 12, &|_| Gen::Counter64 { base: 100, rate: 50 }, &mut tree);                         // ifHCOutMulticastPkts
        col(IFX_TABLE, 13, &|_| Gen::Counter64 { base: 100, rate: 20 }, &mut tree);                         // ifHCOutBroadcastPkts
        col(IFX_TABLE, 15, &|_| Gen::Gauge32(1000), &mut tree);                                             // ifHighSpeed = 1000 Mbps

        Mib { tree }
    }

    // The (oid, value) successors strictly greater than `after`, up to `max`.
    fn successors(&self, after: &[u64], max: usize, elapsed_s: u64) -> Vec<(Vec<u64>, Value)> {
        self.tree
            .range(after.to_vec()..)
            .filter(|(oid, _)| oid.as_slice() != after)
            .take(max)
            .map(|(oid, gen)| (oid.clone(), gen.value(elapsed_s)))
            .collect()
    }

    fn exact(&self, oid: &[u64], elapsed_s: u64) -> Option<Value> {
        self.tree.get(oid).map(|g| g.value(elapsed_s))
    }
}

// ---- minimal BER encoder (SNMP response) -----------------------------------

fn ber_len(len: usize) -> Vec<u8> {
    if len < 0x80 {
        vec![len as u8]
    } else {
        let bytes = len.to_be_bytes();
        let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len() - 1);
        let sig = &bytes[start..];
        let mut out = Vec::with_capacity(sig.len() + 1);
        out.push(0x80 | sig.len() as u8);
        out.extend_from_slice(sig);
        out
    }
}

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len() + 4);
    out.push(tag);
    out.extend(ber_len(content.len()));
    out.extend_from_slice(content);
    out
}

// Minimal signed two's-complement INTEGER content.
fn enc_int_content(mut n: i64) -> Vec<u8> {
    if n == 0 {
        return vec![0];
    }
    let mut bytes = Vec::new();
    let negative = n < 0;
    // Collect big-endian bytes.
    let raw = n.to_be_bytes();
    let _ = &mut n;
    // Trim redundant leading 0x00 (positive) / 0xff (negative) while preserving sign bit.
    let mut i = 0;
    while i < raw.len() - 1
        && ((raw[i] == 0x00 && raw[i + 1] & 0x80 == 0) || (raw[i] == 0xff && raw[i + 1] & 0x80 != 0))
    {
        i += 1;
    }
    bytes.extend_from_slice(&raw[i..]);
    let _ = negative;
    bytes
}

// Unsigned integer content for application types (Counter/Gauge/Counter64):
// big-endian, minimal, with a leading 0x00 if the top bit would be set.
fn enc_uint_content(n: u64) -> Vec<u8> {
    if n == 0 {
        return vec![0];
    }
    let raw = n.to_be_bytes();
    let start = raw.iter().position(|&b| b != 0).unwrap();
    let mut out = Vec::new();
    if raw[start] & 0x80 != 0 {
        out.push(0x00);
    }
    out.extend_from_slice(&raw[start..]);
    out
}

fn enc_oid_content(oid: &[u64]) -> Vec<u8> {
    let mut out = Vec::new();
    // First two sub-identifiers combine into one byte (40*a + b).
    let first = if oid.len() >= 2 { 40 * oid[0] + oid[1] } else { oid.first().copied().unwrap_or(0) };
    encode_base128(first, &mut out);
    for &c in oid.iter().skip(2) {
        encode_base128(c, &mut out);
    }
    out
}

fn encode_base128(mut v: u64, out: &mut Vec<u8>) {
    let mut stack = [0u8; 10];
    let mut n = 0;
    stack[n] = (v & 0x7f) as u8;
    n += 1;
    v >>= 7;
    while v > 0 {
        stack[n] = ((v & 0x7f) as u8) | 0x80;
        n += 1;
        v >>= 7;
    }
    for i in (0..n).rev() {
        out.push(stack[i]);
    }
}

fn enc_value(v: &Value) -> Vec<u8> {
    match v {
        Value::Integer(n) => tlv(0x02, &enc_int_content(*n)),
        Value::OctetString(b) => tlv(0x04, b),
        Value::Counter32(n) => tlv(0x41, &enc_uint_content(*n as u64)),
        Value::Gauge32(n) => tlv(0x42, &enc_uint_content(*n as u64)),
        Value::Counter64(n) => tlv(0x46, &enc_uint_content(*n)),
        Value::EndOfMibView => vec![0x82, 0x00],
    }
}

fn enc_varbind(oid: &[u64], v: &Value) -> Vec<u8> {
    let mut content = tlv(0x06, &enc_oid_content(oid));
    content.extend(enc_value(v));
    tlv(0x30, &content)
}

// Full SNMP v2c Response message.
fn build_response(community: &[u8], req_id: i32, varbinds: &[(Vec<u64>, Value)]) -> Vec<u8> {
    let mut vb_list = Vec::new();
    for (oid, v) in varbinds {
        vb_list.extend(enc_varbind(oid, v));
    }
    let vb_seq = tlv(0x30, &vb_list);

    let mut pdu_body = Vec::new();
    pdu_body.extend(tlv(0x02, &enc_int_content(req_id as i64))); // request-id
    pdu_body.extend(tlv(0x02, &enc_int_content(0)));            // error-status
    pdu_body.extend(tlv(0x02, &enc_int_content(0)));            // error-index
    pdu_body.extend(vb_seq);
    let pdu = tlv(0xa2, &pdu_body); // Response PDU (context-specific constructed 2)

    let mut msg = Vec::new();
    msg.extend(tlv(0x02, &enc_int_content(1))); // version = v2c (1)
    msg.extend(tlv(0x04, community));           // community
    msg.extend(pdu);
    tlv(0x30, &msg)
}

// ---- request handling ------------------------------------------------------

fn oid_components(oid: &snmp2::Oid) -> Vec<u64> {
    oid.iter().map(|it| it.collect()).unwrap_or_default()
}

// Standard multi-OID GETBULK: for each requested OID take its up-to-`max`
// lexical successors, then emit them grouped by repetition (one varbind per
// requested OID per repetition), padding an exhausted OID with endOfMibView so
// the client's positional (repetition-major) de-interleaving stays aligned.
fn bulk_response(mib: &Mib, requested: &[Vec<u64>], max: usize, elapsed_s: u64) -> Vec<(Vec<u64>, Value)> {
    let per: Vec<Vec<(Vec<u64>, Value)>> = requested.iter().map(|o| mib.successors(o, max, elapsed_s)).collect();
    let reps = per.iter().map(|p| p.len()).max().unwrap_or(0);
    let mut out = Vec::new();
    for rep in 0..reps {
        for (i, p) in per.iter().enumerate() {
            if rep < p.len() {
                out.push(p[rep].clone());
            } else {
                let last = p.last().map(|(o, _)| o.clone()).unwrap_or_else(|| requested[i].clone());
                out.push((last, Value::EndOfMibView));
            }
        }
    }
    out
}

// Produce the response bytes for one request datagram, or None if it can't be
// parsed / isn't a request we serve.
fn handle_request(mib: &Mib, start: Instant, datagram: &[u8]) -> Option<Vec<u8>> {
    let pdu = snmp2::Pdu::from_bytes(datagram).ok()?;
    let community = pdu.community.to_vec();
    let req_id = pdu.req_id;
    let elapsed_s = start.elapsed().as_secs();

    // Requested OID(s). GET/GETNEXT use the first; GETBULK walks all of them
    // (the embedded client sends one varbind per table column).
    let requested: Vec<Vec<u64>> = pdu.varbinds.clone().map(|(oid, _)| oid_components(&oid)).collect();
    let first = requested.first().cloned().unwrap_or_default();

    let varbinds: Vec<(Vec<u64>, Value)> = match pdu.message_type {
        snmp2::MessageType::GetRequest => match mib.exact(&first, elapsed_s) {
            Some(v) => vec![(first, v)],
            None => vec![(first, Value::EndOfMibView)],
        },
        snmp2::MessageType::GetNextRequest => {
            let mut succ = mib.successors(&first, 1, elapsed_s);
            if succ.is_empty() {
                vec![(first, Value::EndOfMibView)]
            } else {
                vec![succ.remove(0)]
            }
        }
        snmp2::MessageType::GetBulkRequest => {
            // GETBULK reuses the error-index slot for max-repetitions.
            let max = pdu.error_index.max(1) as usize;
            let out = bulk_response(mib, &requested, max, elapsed_s);
            if out.is_empty() {
                vec![(first, Value::EndOfMibView)]
            } else {
                out
            }
        }
        _ => return None,
    };

    Some(build_response(&community, req_id, &varbinds))
}

fn serve_switch(ip: String, port: u16, mib: Arc<Mib>, start: Instant) {
    let addr = format!("{}:{}", ip, port);
    let socket = match UdpSocket::bind(&addr) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[snmpsim] bind {} failed: {} (on macOS run setup-loopback-macos.sh)", addr, e);
            return;
        }
    };
    let mut buf = [0u8; 65535];
    loop {
        match socket.recv_from(&mut buf) {
            Ok((n, peer)) => {
                if let Some(response) = handle_request(&mib, start, &buf[..n]) {
                    let _ = socket.send_to(&response, peer);
                }
            }
            Err(e) => {
                eprintln!("[snmpsim] recv on {} error: {}", addr, e);
                return;
            }
        }
    }
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn main() {
    // Config via env so the harness drives it uniformly.
    //   SNMPSIM_SWITCHES   number of simulated switches (default 250)
    //   SNMPSIM_INTERFACES interfaces per switch (default 16 -> 4000 total)
    //   SNMPSIM_PORT       UDP port (default 16100)
    //   SNMPSIM_BASE_OCTET first loopback last-octet (default 2 -> 127.0.0.2..)
    let switches = env_u32("SNMPSIM_SWITCHES", 250);
    let interfaces = env_u32("SNMPSIM_INTERFACES", 16);
    let port = env_u32("SNMPSIM_PORT", 16100) as u16;
    let base_octet = env_u32("SNMPSIM_BASE_OCTET", 2);

    let mib = Arc::new(Mib::build(interfaces));
    let start = Instant::now();
    println!(
        "[snmpsim] {} switches x {} interfaces ({} total), port {}, ips 127.0.0.{}..{}",
        switches,
        interfaces,
        switches * interfaces,
        port,
        base_octet,
        base_octet + switches - 1
    );

    let mut handles = Vec::new();
    for n in 0..switches {
        let last = base_octet + n;
        // 127.0.0.0/8 is entirely loopback on Linux; keep within a /8 by rolling
        // the third octet once the last octet passes 255.
        let ip = format!("127.0.{}.{}", last / 256, last % 256);
        let mib = mib.clone();
        let handle = std::thread::spawn(move || serve_switch(ip, port, mib, start));
        handles.push(handle);
    }
    for h in handles {
        let _ = h.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The core guarantee: our hand-rolled encoder produces PDUs that snmp2 (the
    // library nexus uses) parses back to the same OIDs and values.
    #[test]
    fn response_round_trips_through_snmp2() {
        let vbs = vec![
            (vec![1, 3, 6, 1, 2, 1, 2, 2, 1, 8, 1], Value::Integer(1)),
            (vec![1, 3, 6, 1, 2, 1, 31, 1, 1, 1, 6, 1], Value::Counter64(123_456_789_012)),
            (vec![1, 3, 6, 1, 2, 1, 31, 1, 1, 1, 15, 1], Value::Gauge32(1000)),
            (vec![1, 3, 6, 1, 2, 1, 2, 2, 1, 2, 1], Value::OctetString(b"Gi0/1".to_vec())),
        ];
        let bytes = build_response(b"public", 42, &vbs);
        let pdu = snmp2::Pdu::from_bytes(&bytes).expect("snmp2 parses our response");
        assert_eq!(pdu.req_id, 42);
        assert_eq!(pdu.community, b"public");
        assert_eq!(pdu.message_type, snmp2::MessageType::Response);

        let parsed: Vec<_> = pdu.varbinds.clone().collect();
        assert_eq!(parsed.len(), 4);
        // ifOperStatus = 1
        assert_eq!(oid_components(&parsed[0].0), vec![1, 3, 6, 1, 2, 1, 2, 2, 1, 8, 1]);
        assert!(matches!(parsed[0].1, snmp2::Value::Integer(1)));
        // ifHCInOctets = Counter64
        assert!(matches!(parsed[1].1, snmp2::Value::Counter64(123_456_789_012)));
        // ifHighSpeed = Gauge32
        assert!(matches!(parsed[2].1, snmp2::Value::Unsigned32(1000)));
        // ifDescr octet string
        match &parsed[3].1 {
            snmp2::Value::OctetString(b) => assert_eq!(*b, b"Gi0/1"),
            other => panic!("expected octet string, got {:?}", other),
        }
    }

    #[test]
    fn getbulk_walk_returns_sorted_successors() {
        let mib = Mib::build(4);
        // Walk from the ifOperStatus column base: expect 4 rows (indices 1..4),
        // then the walk crosses into the next column.
        let base = vec![1, 3, 6, 1, 2, 1, 2, 2, 1, 8];
        let rows = mib.successors(&base, 20, 0);
        let oper: Vec<_> = rows
            .iter()
            .filter(|(oid, _)| oid.starts_with(&base) && oid.len() == base.len() + 1)
            .collect();
        assert_eq!(oper.len(), 4, "one ifOperStatus row per interface");
        for (_, v) in oper {
            assert!(matches!(v, Value::Integer(1)));
        }
    }

    #[test]
    fn bulk_interleaves_columns_by_repetition() {
        // Two columns (ifDescr .2, ifOperStatus .8) over a 3-interface table,
        // max_repetitions=2. Expect repetition-major order: for each of the 2
        // reps, one varbind per requested column.
        let mib = Mib::build(3);
        let descr = vec![1, 3, 6, 1, 2, 1, 2, 2, 1, 2];
        let status = vec![1, 3, 6, 1, 2, 1, 2, 2, 1, 8];
        let out = bulk_response(&mib, &[descr.clone(), status.clone()], 2, 0);
        assert_eq!(out.len(), 4); // 2 reps x 2 columns
        // rep 0: descr.1 then status.1
        assert_eq!(out[0].0, [descr.as_slice(), &[1]].concat());
        assert_eq!(out[1].0, [status.as_slice(), &[1]].concat());
        // rep 1: descr.2 then status.2
        assert_eq!(out[2].0, [descr.as_slice(), &[2]].concat());
        assert_eq!(out[3].0, [status.as_slice(), &[2]].concat());
        assert!(matches!(out[1].1, Value::Integer(1)));
    }

    #[test]
    fn bulk_pads_exhausted_column_with_end_of_mib() {
        // ifHighSpeed (.15) is the last column in the whole tree, so it has only
        // `interfaces` successors; ifDescr (.2) keeps yielding (its walk runs on
        // into later columns). With more repetitions than ifHighSpeed has rows,
        // ifHighSpeed's slots must pad with endOfMibView to stay aligned.
        let mib = Mib::build(2);
        let descr = vec![1, 3, 6, 1, 2, 1, 2, 2, 1, 2];
        let highspeed = vec![1, 3, 6, 1, 2, 1, 31, 1, 1, 1, 15];
        let out = bulk_response(&mib, &[descr, highspeed], 4, 0);
        assert_eq!(out.len(), 8); // 4 reps x 2 columns, padded
        // ifHighSpeed is column index 1: positions 1,3,5,7. Rows exist for reps
        // 0,1; reps 2,3 (positions 5,7) are padded.
        assert!(matches!(out[5].1, Value::EndOfMibView));
        assert!(matches!(out[7].1, Value::EndOfMibView));
    }

    #[test]
    fn counters_increase_with_time() {
        let mib = Mib::build(1);
        let oid = vec![1, 3, 6, 1, 2, 1, 31, 1, 1, 1, 6, 1]; // ifHCInOctets.1
        let v0 = match mib.exact(&oid, 0).unwrap() {
            Value::Counter64(n) => n,
            _ => panic!(),
        };
        let v10 = match mib.exact(&oid, 10).unwrap() {
            Value::Counter64(n) => n,
            _ => panic!(),
        };
        assert!(v10 > v0, "counter must increase over elapsed time");
    }

    #[test]
    fn uint_content_adds_sign_padding() {
        // 0x80 has the top bit set: must be prefixed with 0x00 as an unsigned value.
        assert_eq!(enc_uint_content(0x80), vec![0x00, 0x80]);
        assert_eq!(enc_uint_content(0x7f), vec![0x7f]);
        assert_eq!(enc_uint_content(0), vec![0]);
    }

    #[test]
    fn oid_first_byte_is_combined() {
        // 1.3.* -> 40*1 + 3 = 43 (0x2b).
        let c = enc_oid_content(&[1, 3, 6, 1]);
        assert_eq!(c[0], 0x2b);
        assert_eq!(&c[1..], &[6, 1]);
    }
}
