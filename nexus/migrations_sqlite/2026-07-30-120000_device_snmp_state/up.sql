-- SQLite twin of migrations/2026-07-30-120000_device_snmp_state (see that dir
-- for the design notes and the pairwise-migration rule).
CREATE TABLE device_snmp_state (
    fqdn varchar PRIMARY KEY NOT NULL,
    ewma_ms double DEFAULT NULL,
    effective_ms bigint NOT NULL,
    consec_timeouts integer NOT NULL,
    dead boolean NOT NULL,
    updated_at bigint NOT NULL
);
