-- Persistent per-VLAN "expected roots" baseline. Some VLANs legitimately have
-- more than one spanning-tree root by design (e.g. a leftover bridge that is no
-- longer in production). Marking such a root as expected suppresses the
-- "STP: multiple roots" issue for that specific tree; the alert only fires when
-- more than one *unacknowledged* root remains, so a genuinely new/unexpected
-- root (or a different device becoming root) re-alerts on its own.
--
-- Identity is (vlan, root_fqdn): build_stp_tree lists roots by fqdn, so a
-- different device becoming root is a different key and is not covered by an
-- existing acknowledgement. Unlike issue_acks these rows are intentional config
-- and are NOT garbage-collected — if the root disappears and later returns, the
-- acknowledgement still applies. root_mac is a display/debug snapshot only.
CREATE TABLE stp_expected_roots (
    vlan BIGINT NOT NULL,
    root_fqdn VARCHAR NOT NULL,
    root_mac VARCHAR,
    acked_at BIGINT NOT NULL,
    acked_by VARCHAR,
    note VARCHAR,
    PRIMARY KEY (vlan, root_fqdn)
);
