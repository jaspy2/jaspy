-- Learned per-device SNMP timeout state for the embedded client's adaptive
-- timeout. Persisted so a known-slow (high-CPU) switch reopens at its learned
-- timeout after a restart instead of re-ramping through timeouts. Keyed by the
-- device fqdn (its primary polling session). ewma_ms is the smoothed observed
-- RTT (null until first success), effective_ms the timeout the next socket
-- open uses, dead marks a device collapsed to fast-fail; updated_at is epoch-ms.
CREATE TABLE device_snmp_state (
    fqdn VARCHAR PRIMARY KEY NOT NULL,
    ewma_ms DOUBLE PRECISION,
    effective_ms BIGINT NOT NULL,
    consec_timeouts INTEGER NOT NULL,
    dead BOOLEAN NOT NULL,
    updated_at BIGINT NOT NULL
);
