-- SQLite twin of migrations/2026-07-30-000000_stp_expected_roots (see that dir
-- for the rationale and the pairwise-migration rule).
CREATE TABLE stp_expected_roots (
    vlan bigint NOT NULL,
    root_fqdn varchar NOT NULL,
    root_mac varchar DEFAULT NULL,
    acked_at bigint NOT NULL,
    acked_by varchar DEFAULT NULL,
    note varchar DEFAULT NULL,
    PRIMARY KEY (vlan, root_fqdn)
);
