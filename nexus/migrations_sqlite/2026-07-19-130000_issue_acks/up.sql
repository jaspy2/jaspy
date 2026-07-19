-- SQLite twin of migrations/2026-07-19-130000_issue_acks (see that dir and the
-- consolidated initial sqlite migration for the pairwise-migration rule).
CREATE TABLE issue_acks (
    issue_key varchar PRIMARY KEY NOT NULL,
    first_seen bigint NOT NULL,
    acked_at bigint NOT NULL,
    acked_by varchar DEFAULT NULL,
    note varchar DEFAULT NULL
);
