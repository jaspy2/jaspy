-- Your SQL goes here

ALTER TABLE devices ADD COLUMN device_type varchar DEFAULT NULL;
ALTER TABLE devices ADD COLUMN software_version varchar DEFAULT NULL;

