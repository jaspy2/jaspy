table! {
    devices (id) {
        id -> Int4,
        name -> Varchar,
        dns_domain -> Varchar,
        snmp_community -> Nullable<Varchar>,
        base_mac -> Nullable<Varchar>,
        polling_enabled -> Nullable<Bool>,
        os_info -> Nullable<Varchar>,
    }
}

table! {
    interfaces (id) {
        id -> Int4,
        index -> Int4,
        interface_type -> Varchar,
        connected_interface -> Nullable<Int4>,
        device_id -> Int4,
        display_name -> Nullable<Varchar>,
        name -> Varchar,
        alias -> Nullable<Varchar>,
        description -> Nullable<Varchar>,
        polling_enabled -> Nullable<Bool>,
        speed_override -> Nullable<Int4>,
    }
}

joinable!(interfaces -> devices (device_id));

allow_tables_to_appear_in_same_query!(
    devices,
    interfaces,
);
