-- Your SQL goes here
CREATE TABLE client_locations (
    id serial PRIMARY KEY NOT NULL,
    device_id int NOT NULL REFERENCES devices(id),
    ip_address varchar NOT NULL,
    port_info varchar NOT NULL
);

