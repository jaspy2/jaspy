-- Consolidated SQLite schema, equivalent to the final state of the postgres
-- migration chain (see migrations/). SQLite gets a single initial migration:
-- there are no pre-existing sqlite deployments to upgrade, and SQLite cannot
-- ALTER TABLE ADD COLUMN with a REFERENCES clause, so mirroring the postgres
-- history 1:1 is not possible anyway. Future schema changes must be added
-- pairwise: one migration dir here and one under migrations/.
--
-- Must stay in sync with src/schema.rs; the dbo unit tests run against an
-- in-memory sqlite database created from this file, so drift fails the unit
-- suite.

CREATE TABLE devices (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name varchar NOT NULL,
  dns_domain varchar NOT NULL,
  snmp_community varchar DEFAULT NULL,
  base_mac varchar DEFAULT NULL,
  polling_enabled boolean DEFAULT NULL,
  os_info varchar DEFAULT NULL,
  device_type varchar DEFAULT NULL,
  software_version varchar DEFAULT NULL
);

CREATE TABLE interfaces (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  -- INDEX is a keyword in sqlite; postgres accepted it bare.
  "index" integer NOT NULL,
  interface_type varchar NOT NULL,
  connected_interface integer REFERENCES interfaces(id) DEFAULT NULL,
  device_id integer REFERENCES devices(id) NOT NULL,
  display_name varchar DEFAULT NULL,
  name varchar NOT NULL,
  alias varchar DEFAULT NULL,
  description varchar DEFAULT NULL,
  polling_enabled boolean DEFAULT NULL,
  speed_override int DEFAULT NULL,
  virtual_connection integer REFERENCES interfaces(id) DEFAULT NULL
);

CREATE TABLE weathermap_device_infos (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  x float NOT NULL,
  y float NOT NULL,
  super_node boolean NOT NULL,
  expanded_by_default boolean NOT NULL,
  device_id integer REFERENCES devices(id) NOT NULL
);

CREATE TABLE client_locations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  device_id int NOT NULL REFERENCES devices(id),
  ip_address varchar NOT NULL,
  port_info varchar NOT NULL,
  hw_address varchar NOT NULL DEFAULT ''
);

CREATE UNIQUE INDEX client_locations_unique_ip_address ON client_locations (ip_address);

CREATE TABLE settings (
  name VARCHAR PRIMARY KEY NOT NULL,
  value VARCHAR NOT NULL
);
