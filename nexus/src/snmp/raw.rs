// Owned mirror of snmp2::Value. snmp2's Value<'a> borrows from the
// session receive buffer, which is reused on the next request, so every value
// pulled off a walk must be copied into an owned form immediately. Keeping our
// own enum also decouples the rendering layer (render.rs) from the snmp2 value
// types, so those render rules are unit-testable without a live session.

#[derive(Debug, Clone, PartialEq)]
pub enum RawValue {
    Boolean(bool),
    Integer(i64),
    OctetString(Vec<u8>),
    Oid(Vec<u64>),
    IpAddress([u8; 4]),
    Counter32(u32),
    Unsigned32(u32),
    Timeticks(u32),
    Opaque(Vec<u8>),
    Counter64(u64),
    Null,
    EndOfMibView,
    NoSuchObject,
    NoSuchInstance,
    // A value snmp2 exposes that we don't map (nested SEQUENCE/SET, PDU
    // markers). Treated as an absent/empty value downstream.
    Unsupported,
}

impl RawValue {
    // True for the sentinels that terminate a column walk.
    pub fn is_end_of_view(&self) -> bool {
        matches!(self, RawValue::EndOfMibView | RawValue::NoSuchObject | RawValue::NoSuchInstance)
    }
}

// Convert a borrowed snmp2 value into our owned form. Kept here (behind the
// snmp2 dependency) so raw.rs is the single seam between snmp2 and the rest of
// the module.
pub fn from_snmp2(value: &snmp2::Value) -> RawValue {
    use snmp2::Value as V;
    match value {
        V::Boolean(b) => RawValue::Boolean(*b),
        V::Integer(i) => RawValue::Integer(*i),
        V::OctetString(bytes) => RawValue::OctetString(bytes.to_vec()),
        V::ObjectIdentifier(oid) => match oid.iter() {
            Some(it) => RawValue::Oid(it.collect()),
            None => RawValue::Unsupported,
        },
        V::IpAddress(octets) => RawValue::IpAddress(*octets),
        V::Counter32(v) => RawValue::Counter32(*v),
        V::Unsigned32(v) => RawValue::Unsigned32(*v),
        V::Timeticks(v) => RawValue::Timeticks(*v),
        V::Opaque(bytes) => RawValue::Opaque(bytes.to_vec()),
        V::Counter64(v) => RawValue::Counter64(*v),
        V::Null => RawValue::Null,
        V::EndOfMibView => RawValue::EndOfMibView,
        V::NoSuchObject => RawValue::NoSuchObject,
        V::NoSuchInstance => RawValue::NoSuchInstance,
        _ => RawValue::Unsupported,
    }
}
