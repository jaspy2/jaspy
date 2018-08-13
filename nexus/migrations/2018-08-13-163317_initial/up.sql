-- Your SQL goes here
CREATE TABLE devices (
  id serial PRIMARY KEY,
  name varchar NOT NULL,
  dns_domain varchar NOT NULL,
  snmp_community varchar DEFAULT NULL,
  base_mac varchar DEFAULT NULL,
  polling_enabled boolean DEFAULT NULL,
  os_info varchar DEFAULT NULL
);

CREATE TABLE interfaces (
  id serial PRIMARY KEY,
  index integer NOT NULL,
  interface_type varchar NOT NULL,
  connected_interface integer REFERENCES interfaces(id) DEFAULT NULL,
  device_id integer REFERENCES devices(id) NOT NULL,
  display_name varchar DEFAULT NULL,
  name varchar NOT NULL,
  alias varchar DEFAULT NULL,
  description varchar DEFAULT NULL,
  polling_enabled boolean DEFAULT NULL,
  speed_override int DEFAULT NULL
);