// Value rendering + table index decoding for the embedded client. The goal is
// byte-for-byte parity with what snmpbot emits (and therefore with what the
// collectors already deserialize), so these rules are cross-checked against
// the captured fixtures in tests/fixtures/*.json.
use super::mib::{MibObject, Syntax};
use super::raw::RawValue;
use super::types::SNMPBotResultEntryObjectValue as Val;
use std::collections::HashMap;
use std::sync::Arc;

// Render one SNMP value according to its MIB syntax, matching snmpbot's JSON
// encoding.
pub fn render_value(raw: &RawValue, syntax: &Syntax) -> Val {
    // The no-value sentinels render as Empty regardless of declared syntax.
    if raw.is_end_of_view() || matches!(raw, RawValue::Null) {
        return Val::Empty;
    }

    match syntax {
        Syntax::Unsigned => match raw_as_u64(raw) {
            Some(v) => Val::Uint64(v),
            None => Val::Empty,
        },
        Syntax::Integer => match raw_as_i64(raw) {
            Some(v) if v >= 0 => Val::Uint64(v as u64),
            // Negative INTEGERs deserialize into Float64 under the untagged
            // enum (there is no signed variant); entPhySensorValue can be
            // negative and the collectors accept either number variant.
            Some(v) => Val::Float64(v as f64),
            None => Val::Empty,
        },
        Syntax::Enum(names) => match raw_as_i64(raw) {
            Some(v) => match names.get(&v) {
                Some(name) => Val::Str(name.clone()),
                None if v >= 0 => Val::Uint64(v as u64),
                None => Val::Float64(v as f64),
            },
            None => Val::Empty,
        },
        Syntax::DisplayString => match raw_bytes(raw) {
            Some(bytes) => Val::Str(display_string(bytes)),
            None => Val::Empty,
        },
        Syntax::MacAddress => match raw_bytes(raw) {
            Some(bytes) => Val::Str(hex_colons(bytes)),
            None => Val::Empty,
        },
        Syntax::OctetString => match raw_bytes(raw) {
            Some(bytes) => Val::Str(hex_spaces(bytes)),
            None => Val::Empty,
        },
        Syntax::IpAddress => match raw {
            RawValue::IpAddress(o) => Val::Str(format!("{}.{}.{}.{}", o[0], o[1], o[2], o[3])),
            RawValue::OctetString(b) if b.len() == 4 => Val::Str(format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3])),
            _ => Val::Empty,
        },
        Syntax::ObjectIdentifier => match raw {
            RawValue::Oid(parts) => Val::Str(parts.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(".")),
            _ => Val::Empty,
        },
        Syntax::Bits(bit_names) => match raw_bytes(raw) {
            Some(bytes) => Val::Other(serde_json::Value::Array(
                decode_bits(bytes, bit_names).into_iter().map(serde_json::Value::String).collect(),
            )),
            None => Val::Empty,
        },
    }
}

fn raw_as_u64(raw: &RawValue) -> Option<u64> {
    match raw {
        RawValue::Integer(v) if *v >= 0 => Some(*v as u64),
        RawValue::Counter32(v) | RawValue::Unsigned32(v) | RawValue::Timeticks(v) => Some(*v as u64),
        RawValue::Counter64(v) => Some(*v),
        _ => None,
    }
}

fn raw_as_i64(raw: &RawValue) -> Option<i64> {
    match raw {
        RawValue::Integer(v) => Some(*v),
        RawValue::Counter32(v) | RawValue::Unsigned32(v) | RawValue::Timeticks(v) => Some(*v as i64),
        RawValue::Counter64(v) => Some(*v as i64),
        _ => None,
    }
}

fn raw_bytes(raw: &RawValue) -> Option<&[u8]> {
    match raw {
        RawValue::OctetString(b) | RawValue::Opaque(b) => Some(b),
        _ => None,
    }
}

// DisplayString: UTF-8 (lossy), with a single trailing NUL stripped (some
// agents NUL-terminate). snmpbot renders these as plain strings.
fn display_string(bytes: &[u8]) -> String {
    let end = if bytes.last() == Some(&0) { bytes.len() - 1 } else { bytes.len() };
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

// "aa:bb:cc:dd:ee:ff" — lowercase, colon-separated.
fn hex_colons(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(":")
}

// "7f ff ff ..." — lowercase, space-separated. snmpbot's rendering of raw
// OCTET STRINGs (e.g. VLAN PortList bitmaps).
fn hex_spaces(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ")
}

// BITS: bit N lives in byte N/8, MSB first (bit 0 = 0x80 of byte 0). Emit the
// set bits' names in ascending bit order (bit_names is pre-sorted by bit).
fn decode_bits(bytes: &[u8], bit_names: &[(u32, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (bit, name) in bit_names {
        let byte_idx = (*bit / 8) as usize;
        let mask = 0x80u8 >> (*bit % 8);
        if byte_idx < bytes.len() && bytes[byte_idx] & mask != 0 {
            out.push(name.clone());
        }
    }
    out
}

// Decode a table row's OID index suffix into the {index-object-id -> i64} map
// the collectors expect. Every table the collectors use is indexed purely by
// integer-family objects, each consuming exactly one sub-identifier. A
// non-integer index object, or a suffix whose length doesn't match, yields
// None (the row is skipped).
pub fn decode_index(suffix: &[u64], index_objs: &[Arc<MibObject>]) -> Option<HashMap<String, i64>> {
    if suffix.len() != index_objs.len() {
        return None;
    }
    let mut out = HashMap::new();
    for (obj, sub) in index_objs.iter().zip(suffix.iter()) {
        match obj.syntax {
            Syntax::Integer | Syntax::Unsigned | Syntax::Enum(_) => {
                out.insert(obj.id.clone(), *sub as i64);
            }
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enum_syntax() -> Syntax {
        let mut m = HashMap::new();
        m.insert(1, "up".to_string());
        m.insert(2, "down".to_string());
        m.insert(6, "ethernetCsmacd".to_string());
        Syntax::Enum(m)
    }

    #[test]
    fn unsigned_and_integer_number_variants() {
        assert!(matches!(render_value(&RawValue::Counter64(1000000), &Syntax::Unsigned), Val::Uint64(1000000)));
        assert!(matches!(render_value(&RawValue::Counter32(5), &Syntax::Unsigned), Val::Uint64(5)));
        assert!(matches!(render_value(&RawValue::Timeticks(2297973), &Syntax::Unsigned), Val::Uint64(2297973)));
        assert!(matches!(render_value(&RawValue::Integer(42), &Syntax::Integer), Val::Uint64(42)));
        // Negative INTEGER -> Float64 (matches untagged-enum deserialization).
        match render_value(&RawValue::Integer(-40), &Syntax::Integer) {
            Val::Float64(v) => assert_eq!(v, -40.0),
            _ => panic!("expected Float64"),
        }
    }

    #[test]
    fn enum_renders_name_then_falls_back_to_number() {
        assert!(matches!(render_value(&RawValue::Integer(1), &enum_syntax()), Val::Str(ref s) if s == "up"));
        assert!(matches!(render_value(&RawValue::Integer(6), &enum_syntax()), Val::Str(ref s) if s == "ethernetCsmacd"));
        // Unnamed enum value -> number.
        assert!(matches!(render_value(&RawValue::Integer(99), &enum_syntax()), Val::Uint64(99)));
    }

    #[test]
    fn display_string_strips_trailing_nul() {
        assert!(matches!(render_value(&RawValue::OctetString(b"Gig0/1".to_vec()), &Syntax::DisplayString), Val::Str(ref s) if s == "Gig0/1"));
        assert!(matches!(render_value(&RawValue::OctetString(b"Gig0/1\0".to_vec()), &Syntax::DisplayString), Val::Str(ref s) if s == "Gig0/1"));
    }

    #[test]
    fn mac_address_colon_hex() {
        let bytes = vec![0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x02];
        assert!(matches!(render_value(&RawValue::OctetString(bytes), &Syntax::MacAddress), Val::Str(ref s) if s == "aa:bb:cc:dd:ee:02"));
    }

    #[test]
    fn raw_octet_string_space_hex() {
        // Matches the vlantrunkporttable / jaspystpbridgetable fixture form.
        let bytes = vec![0x7f, 0xff, 0x00];
        assert!(matches!(render_value(&RawValue::OctetString(bytes), &Syntax::OctetString), Val::Str(ref s) if s == "7f ff 00"));
    }

    #[test]
    fn bits_render_as_name_array_ascending() {
        // 0xBC = bits 0,2,3,4,5 set (MSB-first).
        let bit_names = vec![
            (0, "lacpActivity".to_string()),
            (1, "lacpTimeout".to_string()),
            (2, "aggregation".to_string()),
            (3, "synchronization".to_string()),
            (4, "collecting".to_string()),
            (5, "distributing".to_string()),
            (6, "defaulted".to_string()),
            (7, "expired".to_string()),
        ];
        let rendered = render_value(&RawValue::OctetString(vec![0xBC]), &Syntax::Bits(bit_names));
        match rendered {
            Val::Other(serde_json::Value::Array(items)) => {
                let names: Vec<String> = items.into_iter().map(|v| v.as_str().unwrap().to_string()).collect();
                assert_eq!(names, vec!["lacpActivity", "aggregation", "synchronization", "collecting", "distributing"]);
            }
            _ => panic!("expected array"),
        }
    }

    #[test]
    fn sentinels_render_empty() {
        assert!(matches!(render_value(&RawValue::NoSuchObject, &Syntax::Unsigned), Val::Empty));
        assert!(matches!(render_value(&RawValue::EndOfMibView, &Syntax::DisplayString), Val::Empty));
        assert!(matches!(render_value(&RawValue::Null, &Syntax::Integer), Val::Empty));
    }

    fn int_obj(id: &str) -> Arc<MibObject> {
        Arc::new(MibObject { id: id.to_string(), oid: vec![], syntax: Syntax::Integer })
    }

    #[test]
    fn decode_single_integer_index() {
        let objs = vec![int_obj("IF-MIB::ifIndex")];
        let map = decode_index(&[10101], &objs).unwrap();
        assert_eq!(map.get("IF-MIB::ifIndex"), Some(&10101));
    }

    #[test]
    fn decode_multi_integer_index() {
        let objs = vec![int_obj("A::vlanId"), int_obj("A::portIndex")];
        let map = decode_index(&[100, 5], &objs).unwrap();
        assert_eq!(map.get("A::vlanId"), Some(&100));
        assert_eq!(map.get("A::portIndex"), Some(&5));
    }

    #[test]
    fn decode_index_length_mismatch_is_none() {
        let objs = vec![int_obj("A::x"), int_obj("A::y")];
        assert!(decode_index(&[1], &objs).is_none());
        assert!(decode_index(&[1, 2, 3], &objs).is_none());
    }

    #[test]
    fn decode_index_rejects_non_integer_index() {
        let objs = vec![Arc::new(MibObject { id: "A::s".to_string(), oid: vec![], syntax: Syntax::OctetString })];
        assert!(decode_index(&[65], &objs).is_none());
    }
}
