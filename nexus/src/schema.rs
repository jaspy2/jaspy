table! {
    client_locations (id) {
        id -> Int4,
        device_id -> Int4,
        ip_address -> Varchar,
        port_info -> Varchar,
    }
}

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
        virtual_connection -> Nullable<Int4>,
    }
}

table! {
    weathermap_device_infos (id) {
        id -> Int4,
        x -> Float8,
        y -> Float8,
        super_node -> Bool,
        expanded_by_default -> Bool,
        device_id -> Int4,
    }
}

joinable!(client_locations -> devices (device_id));
joinable!(interfaces -> devices (device_id));
joinable!(weathermap_device_infos -> devices (device_id));

allow_tables_to_appear_in_same_query!(
    client_locations,
    devices,
    interfaces,
    weathermap_device_infos,
);
