-- Your SQL goes here

CREATE TABLE weathermap_device_infos (
  id serial PRIMARY KEY,
  x float NOT NULL,
  y float NOT NULL,
  super_node boolean NOT NULL,
  expanded_by_default boolean NOT NULL,
  device_id integer REFERENCES devices(id) NOT NULL
);