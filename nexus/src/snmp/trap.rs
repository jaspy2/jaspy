// Embedded SNMP trap receiver. An optional background thread (config
// `enable_trap_receiver`) that binds a UDP socket, decodes linkUp/linkDown
// traps with snmp2, resolves the source device, and reports the interface
// up/down state straight into IMDS — the same ingest the snmptrapd-driven
// `trap-handler` subcommand uses, but in-process.
//
// The decode logic (`link_event`) is a pure function over owned varbinds so it
// is unit-testable without crafting BER; `link_event_from_pdu` is the thin
// adapter over a parsed snmp2::Pdu.
use super::raw::{from_snmp2, RawValue};
use crate::db;
use crate::models;
use crate::utilities::imds::IMDS;
use crate::utilities::tools;
use snmp2::{MessageType, Pdu};
use std::collections::HashMap;
use std::net::{IpAddr, ToSocketAddrs, UdpSocket};
use std::sync::{atomic, Arc, Mutex};
use std::time::Duration;

// Well-known notification/identity OIDs. The snmpbot MIB JSONs carry objects
// only (no NOTIFICATION-TYPE), so these can't be resolved from the registry
// and are hardcoded, as snmptrapd's translation effectively does.
const OID_SNMP_TRAP_OID: &[u64] = &[1, 3, 6, 1, 6, 3, 1, 1, 4, 1, 0];
const OID_LINK_DOWN: &[u64] = &[1, 3, 6, 1, 6, 3, 1, 1, 5, 3];
const OID_LINK_UP: &[u64] = &[1, 3, 6, 1, 6, 3, 1, 1, 5, 4];
const OID_IF_INDEX: &[u64] = &[1, 3, 6, 1, 2, 1, 2, 2, 1, 1];

pub enum TrapKind {
    // v2c Trap / InformRequest: the notification is the snmpTrapOID.0 varbind.
    V2,
    // v1 Trap: the notification is the generic-trap code (3 = linkUp, 2 = linkDown).
    V1 { generic_trap: i64 },
}

// Link state carried by a trap: (ifIndex, is_up). None if it is not a
// linkUp/linkDown trap or lacks a usable ifIndex. Mirrors traphandler.rs's
// text-based `link_event_from_trap`.
pub fn link_event(kind: &TrapKind, varbinds: &[(Vec<u64>, RawValue)]) -> Option<(i64, bool)> {
    let up = match kind {
        TrapKind::V1 { generic_trap } => match generic_trap {
            3 => true,
            2 => false,
            _ => return None,
        },
        TrapKind::V2 => {
            let trap_oid = varbinds
                .iter()
                .find(|(oid, _)| oid.as_slice() == OID_SNMP_TRAP_OID)
                .map(|(_, value)| value)?;
            match trap_oid {
                RawValue::Oid(components) if components.as_slice() == OID_LINK_UP => true,
                RawValue::Oid(components) if components.as_slice() == OID_LINK_DOWN => false,
                _ => return None,
            }
        }
    };

    let ifindex = varbinds
        .iter()
        .find(|(oid, _)| starts_with(oid, OID_IF_INDEX))
        .and_then(|(oid, value)| ifindex_value(oid, value))?;
    Some((ifindex, up))
}

fn ifindex_value(oid: &[u64], value: &RawValue) -> Option<i64> {
    match value {
        RawValue::Integer(v) => Some(*v),
        RawValue::Counter32(v) | RawValue::Unsigned32(v) | RawValue::Timeticks(v) => Some(*v as i64),
        // Fall back to the instance sub-identifier (ifIndex.<n> = <n>).
        _ => oid.get(OID_IF_INDEX.len()).map(|sub| *sub as i64),
    }
}

fn starts_with(oid: &[u64], prefix: &[u64]) -> bool {
    oid.len() >= prefix.len() && oid[..prefix.len()] == *prefix
}

fn oid_vec(oid: &snmp2::Oid) -> Vec<u64> {
    oid.iter().map(|it| it.collect()).unwrap_or_default()
}

pub fn link_event_from_pdu(pdu: &Pdu) -> Option<(i64, bool)> {
    let kind = match pdu.message_type {
        MessageType::TrapV1 => TrapKind::V1 {
            generic_trap: pdu.v1_trap_info.as_ref().map(|i| i.generic_trap).unwrap_or(-1),
        },
        MessageType::Trap | MessageType::InformRequest => TrapKind::V2,
        _ => return None,
    };
    let varbinds: Vec<(Vec<u64>, RawValue)> =
        pdu.varbinds.clone().map(|(oid, value)| (oid_vec(&oid), from_snmp2(&value))).collect();
    link_event(&kind, &varbinds)
}

// --- receiver thread ------------------------------------------------------

// Map of source IP -> device fqdn, rebuilt periodically by forward-resolving
// the monitored devices (same approach as the discovery engine). Reverse DNS
// is the fallback for a source that doesn't match any monitored device's
// forward record.
struct SourceMap {
    by_ip: HashMap<IpAddr, String>,
}

impl SourceMap {
    fn rebuild(pool: &db::Pool) -> SourceMap {
        let mut by_ip = HashMap::new();
        if let Ok(mut conn) = pool.get() {
            for device in models::dbo::Device::monitored(&mut *conn).iter() {
                let fqdn = format!("{}.{}", device.name, device.dns_domain);
                if let Ok(addrs) = (fqdn.as_str(), 0u16).to_socket_addrs() {
                    for addr in addrs {
                        by_ip.insert(addr.ip(), fqdn.clone());
                    }
                }
            }
        }
        SourceMap { by_ip }
    }

    fn resolve(&self, ip: &IpAddr) -> Option<String> {
        self.by_ip.get(ip).cloned()
    }
}

pub fn run(bind_address: String, imds: Arc<Mutex<IMDS>>, running: Arc<atomic::AtomicBool>) {
    let socket = match UdpSocket::bind(&bind_address) {
        Ok(s) => s,
        Err(e) => {
            // Loud failure: port 162 needs CAP_NET_BIND_SERVICE. A silent
            // return would look like traps simply never arriving.
            println!("[trap] failed to bind {} ({}); trap receiver disabled. Port 162 requires CAP_NET_BIND_SERVICE.", bind_address, e);
            return;
        }
    };
    // Short read timeout so the loop can observe shutdown promptly.
    let _ = socket.set_read_timeout(Some(Duration::from_secs(1)));
    println!("[trap] listening for SNMP traps on {}", bind_address);

    let pool = db::connect();
    let mut sources = SourceMap::rebuild(&pool);
    let mut last_refresh = tools::get_time();
    let mut buf = [0u8; 65536];

    while running.load(atomic::Ordering::Relaxed) {
        // Refresh the source map every 60s so newly added devices resolve.
        let now = tools::get_time();
        if now - last_refresh >= 60.0 {
            sources = SourceMap::rebuild(&pool);
            last_refresh = now;
        }

        let (len, from) = match socket.recv_from(&mut buf) {
            Ok(v) => v,
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(e) => {
                println!("[trap] recv error: {}", e);
                continue;
            }
        };

        let pdu = match Pdu::from_bytes(&buf[..len]) {
            Ok(p) => p,
            Err(e) => {
                println!("[trap] failed to parse trap from {}: {:?}", from.ip(), e);
                continue;
            }
        };

        let (ifindex, up) = match link_event_from_pdu(&pdu) {
            Some(event) => event,
            None => continue, // not a linkUp/linkDown trap, or no ifIndex
        };

        let fqdn = match sources.resolve(&from.ip()) {
            Some(fqdn) => fqdn,
            None => {
                // Refresh once in case the device was added since the last
                // rebuild; otherwise drop the trap (unknown source).
                sources = SourceMap::rebuild(&pool);
                last_refresh = now;
                match sources.resolve(&from.ip()) {
                    Some(fqdn) => fqdn,
                    None => {
                        println!("[trap] dropping trap from unknown source {}", from.ip());
                        continue;
                    }
                }
            }
        };

        println!("[trap] {} event: {} ifIndex {}", if up { "linkup" } else { "linkdown" }, fqdn, ifindex);
        let report = models::json::InterfaceMonitorReport::link_event(&fqdn, ifindex as i32, up);
        if let Ok(mut conn) = pool.get() {
            if let Ok(ref mut imds) = imds.lock() {
                imds.report_interfaces(&mut *conn, report);
            }
        }
    }
    println!("[trap] receiver stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid_vb(oid: &[u64], value: RawValue) -> (Vec<u64>, RawValue) {
        (oid.to_vec(), value)
    }

    #[test]
    fn v2_linkdown_with_ifindex() {
        let varbinds = vec![
            oid_vb(OID_SNMP_TRAP_OID, RawValue::Oid(OID_LINK_DOWN.to_vec())),
            oid_vb(&[1, 3, 6, 1, 2, 1, 2, 2, 1, 1, 10101], RawValue::Integer(10101)),
        ];
        assert_eq!(link_event(&TrapKind::V2, &varbinds), Some((10101, false)));
    }

    #[test]
    fn v2_linkup_with_ifindex() {
        let varbinds = vec![
            oid_vb(OID_SNMP_TRAP_OID, RawValue::Oid(OID_LINK_UP.to_vec())),
            oid_vb(&[1, 3, 6, 1, 2, 1, 2, 2, 1, 1, 7], RawValue::Integer(7)),
        ];
        assert_eq!(link_event(&TrapKind::V2, &varbinds), Some((7, true)));
    }

    #[test]
    fn v2_ifindex_falls_back_to_instance_suffix() {
        // ifIndex varbind present but value is not an integer type.
        let varbinds = vec![
            oid_vb(OID_SNMP_TRAP_OID, RawValue::Oid(OID_LINK_UP.to_vec())),
            oid_vb(&[1, 3, 6, 1, 2, 1, 2, 2, 1, 1, 42], RawValue::Null),
        ];
        assert_eq!(link_event(&TrapKind::V2, &varbinds), Some((42, true)));
    }

    #[test]
    fn v1_generic_trap_codes() {
        let varbinds = vec![oid_vb(&[1, 3, 6, 1, 2, 1, 2, 2, 1, 1, 3], RawValue::Integer(3))];
        assert_eq!(link_event(&TrapKind::V1 { generic_trap: 3 }, &varbinds), Some((3, true)));
        assert_eq!(link_event(&TrapKind::V1 { generic_trap: 2 }, &varbinds), Some((3, false)));
        // Non-link generic trap (e.g. coldStart = 0) is ignored.
        assert_eq!(link_event(&TrapKind::V1 { generic_trap: 0 }, &varbinds), None);
    }

    #[test]
    fn non_link_v2_trap_is_none() {
        let cold_start = vec![1, 3, 6, 1, 6, 3, 1, 1, 5, 1];
        let varbinds = vec![
            oid_vb(OID_SNMP_TRAP_OID, RawValue::Oid(cold_start)),
            oid_vb(&[1, 3, 6, 1, 2, 1, 2, 2, 1, 1, 1], RawValue::Integer(1)),
        ];
        assert_eq!(link_event(&TrapKind::V2, &varbinds), None);
    }

    #[test]
    fn missing_ifindex_is_none() {
        let varbinds = vec![oid_vb(OID_SNMP_TRAP_OID, RawValue::Oid(OID_LINK_UP.to_vec()))];
        assert_eq!(link_event(&TrapKind::V2, &varbinds), None);
    }

    #[test]
    fn missing_trap_oid_is_none() {
        let varbinds = vec![oid_vb(&[1, 3, 6, 1, 2, 1, 2, 2, 1, 1, 1], RawValue::Integer(1))];
        assert_eq!(link_event(&TrapKind::V2, &varbinds), None);
    }

    // --- minimal BER encoder for a real v2c trap, to exercise the actual
    // snmp2 wire decode path (Pdu::from_bytes -> link_event_from_pdu) ---

    fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        // Short form is enough for our tiny PDUs (<128 bytes per field).
        assert!(content.len() < 128, "test encoder only handles short-form lengths");
        out.push(content.len() as u8);
        out.extend_from_slice(content);
        out
    }

    fn enc_int(v: i64) -> Vec<u8> {
        // Minimal two's-complement encoding, good enough for small positives.
        let mut bytes = vec![];
        let mut n = v;
        if n == 0 {
            bytes.push(0);
        } else {
            while n > 0 {
                bytes.insert(0, (n & 0xff) as u8);
                n >>= 8;
            }
            if bytes[0] & 0x80 != 0 {
                bytes.insert(0, 0);
            }
        }
        tlv(0x02, &bytes)
    }

    fn enc_oid_body(components: &[u64]) -> Vec<u8> {
        let mut body = vec![(40 * components[0] + components[1]) as u8];
        for &c in &components[2..] {
            if c < 128 {
                body.push(c as u8);
            } else {
                let mut stack = vec![(c & 0x7f) as u8];
                let mut rest = c >> 7;
                while rest > 0 {
                    stack.push(((rest & 0x7f) | 0x80) as u8);
                    rest >>= 7;
                }
                stack.reverse();
                body.extend_from_slice(&stack);
            }
        }
        body
    }

    fn enc_oid(components: &[u64]) -> Vec<u8> {
        tlv(0x06, &enc_oid_body(components))
    }

    fn enc_varbind(oid: &[u64], value: Vec<u8>) -> Vec<u8> {
        let mut content = enc_oid(oid);
        content.extend(value);
        tlv(0x30, &content)
    }

    fn encode_v2c_linkdown(ifindex: u64) -> Vec<u8> {
        // varbinds: sysUpTime.0 (TimeTicks), snmpTrapOID.0 (OID=linkDown),
        // ifIndex.<n> (INTEGER).
        let mut vbs = vec![];
        vbs.extend(enc_varbind(&[1, 3, 6, 1, 2, 1, 1, 3, 0], tlv(0x43, &[0x00])));
        vbs.extend(enc_varbind(OID_SNMP_TRAP_OID, enc_oid(OID_LINK_DOWN)));
        let mut ifidx_oid = OID_IF_INDEX.to_vec();
        ifidx_oid.push(ifindex);
        vbs.extend(enc_varbind(&ifidx_oid, enc_int(ifindex as i64)));
        let vb_seq = tlv(0x30, &vbs);

        // Trap-PDU [7] { req-id, error-status, error-index, varbinds }.
        let mut pdu = vec![];
        pdu.extend(enc_int(1)); // request-id
        pdu.extend(enc_int(0)); // error-status
        pdu.extend(enc_int(0)); // error-index
        pdu.extend(vb_seq);
        let trap_pdu = tlv(0xa7, &pdu);

        // Message { version=1 (v2c), community, data }.
        let mut msg = vec![];
        msg.extend(enc_int(1));
        msg.extend(tlv(0x04, b"public"));
        msg.extend(trap_pdu);
        tlv(0x30, &msg)
    }

    #[test]
    fn real_snmp2_decode_of_encoded_v2c_trap() {
        let bytes = encode_v2c_linkdown(10101);
        let pdu = Pdu::from_bytes(&bytes).expect("snmp2 should parse our encoded trap");
        assert!(matches!(pdu.message_type, MessageType::Trap));
        assert_eq!(link_event_from_pdu(&pdu), Some((10101, false)));
    }
}
