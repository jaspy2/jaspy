-- Your SQL goes here

ALTER TABLE interfaces ADD COLUMN virtual_connection int REFERENCES interfaces(id) DEFAULT NULL;