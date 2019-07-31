-- This file should undo anything in `up.sql`

ALTER TABLE interfaces DROP COLUMN device_type;
ALTER TABLE interfaces DROP COLUMN software_version;
