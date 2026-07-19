-- Persistent acknowledgements of fleet issues. Issues themselves are derived
-- on the fly from in-memory state (see utilities/issues.rs); only the ack of a
-- specific occurrence is persisted. issue_key is the deterministic composite
-- "<fqdn>|<kind>|<subject>"; first_seen ties the ack to one occurrence so a
-- cleared-then-recurring condition re-alerts (its new first_seen won't match).
CREATE TABLE issue_acks (
    issue_key VARCHAR PRIMARY KEY NOT NULL,
    first_seen BIGINT NOT NULL,
    acked_at BIGINT NOT NULL,
    acked_by VARCHAR,
    note VARCHAR
);
