# jaspy

Jaspy is a set of tools to monitor a network of switches and routers with Prometheus. Specifically it:

* Discovers your network topology using SNMP and stores it in a PostgreSQL database.
* Has a very efficient SNMP poller capable of polling easily a network of several thousands of interface ports every 10 seconds with moderate CPU usage.
* Exposes gathered metrics to Prometheus.
* Offers a few simple command line tools to browse the database.
* Offers a very simple browser based single screen topology viewer in a web browser using JavaScript.

Jaspy allows you to:

* Use Prometheus and Grafana to build dashboards of your network
* Use Prometheus alerting pipeline to build alerts on errors, interface disconnects etc.
* Answer complex metric queries using Prometheus query language.

In order to use Jaspy you will need to have:

* A Fully Qualified Domain Name (fqdn) to each of your network device
* Use LLDP and/or CDP in your network
* Expose snmp 1/2c with a read-only community
* Run Jaspy in a server/VM which has SNMP access to all of your switches
* Each network device must have a hostname as `something.foobar.com` so that the `something` is assumed to be a short name for your switch. It will not work if you dont have working hostnames which resolves to device IP addresses.

## Building Jaspy Debian packages (you need docker for the build)

```
$ cd /jaspy-source
$ cd build
$ ./build-debian.sh
$ ls output/debian/
jaspy_2.2.0-10-gc6fafaf_amd64.deb  snmpbot_2.2.0-10-gc6fafaf_amd64.deb
```

## Installation

These instructions assume that you are have a fresh Debian Buster (10.x) installation. Your VM should be in a network which has access to all of your network switches and it should be secured from outside access. Jaspy does not have any authentication or user management features. 

### Install dependencies:

```
apt-get install mosquitto postgresql prometheus
```

(apache2 is no longer needed: `jaspy-nexus` serves the web admin UI, the API
and the weathermap itself on port 8000.)

### Install Jaspy itself

The installation script should create a `jaspy` database into Postgres if it doesn't yet exists. It should be enough to just install the packages.

```
dpkg -i jaspy_2.2.0-56-gcf420c6_amd64.deb snmpbot_2.2.0-35-g13aada5_amd64.deb
```

### Start jaspy services for the first time

The SNMP interface poller, the ICMP pinger and the entity/STP sensor poller now
run inside `jaspy-nexus` itself (they used to be the separate `jaspy-poller`,
`jaspy-pinger` and `jaspy-entitypoller` services), so there are fewer units to
start:

```
systemctl start jaspy-nexus
systemctl start snmpbot
```

### Web admin UI

`jaspy-nexus` serves a web admin interface at `http://<jaspy-vm>:8000/`. From
there you can see the device inventory and live status, configure and trigger
topology discovery, browse client locations, set the current event name, and
reset the jaspy state between events (Maintenance page). The UI talks to the
`/api/v1/*` endpoints; the `/dev/*` API remains for machine-to-machine
integrations (cli tools, prometheus, snmptrapd, switchmaster).

Note: the UI and API are currently unauthenticated — keep the VM reachable
only from trusted networks. Authentication is planned; until then you can set
`ROCKET_ADDRESS=127.0.0.1` in `jaspy-nexus.service` and front nexus with your
own authenticating reverse proxy if needed.

### Check that everything is working

```
$ systemctl --all list-units 'jaspy*'

UNIT                LOAD   ACTIVE SUB     DESCRIPTION
jaspy-nexus.service loaded active running jaspy-nexus

LOAD   = Reflects whether the unit definition was properly loaded.
ACTIVE = The high-level unit activation state, i.e. generalization of SUB.
SUB    = The low-level unit activation state, values depend on unit type.

1 loaded units listed.
To show all installed unit files use 'systemctl list-unit-files'.
```

### Discover your network topology

Ensure that your switch has snmp enabled: For example in Cisco you would say `snmp-server community public RO`. You should test your snmp with `snmpwalk -c public 172.16.140.200 -v 2c`

The discovery engine runs inside `jaspy-nexus` (it used to be a separate Python
tool). It works by giving it a single root device to start from and it will
iterate recursively over your network:

```
jaspy-discover -c public -r 172.16.140.200 -d foobar.com
```

`jaspy-discover` triggers a run over the nexus HTTP API (`POST
/dev/discovery/run`) and waits for it to finish; progress can be watched at
`GET /dev/discovery/status`. To run discovery periodically without the CLI,
configure defaults on the nexus service and set an interval, e.g. in
`jaspy-nexus.service`:

```
Environment=JASPY_DISCOVERY_ROOT_DEVICE=172.16.140.200
Environment=JASPY_DISCOVERY_COMMUNITY=public
Environment=JASPY_DISCOVERY_DNS_DOMAINS=foobar.com
Environment=JASPY_DISCOVERY_INTERVAL_SECS=3600
```

The same settings can be viewed and changed at runtime from the web UI
(Discovery page) or via `GET`/`PUT /dev/discovery/config`. Saved changes are
persisted in the database and survive restarts — once a config has been saved
this way, it takes precedence over the `JASPY_DISCOVERY_*` environment values
(which act only as the first-boot seed; nexus logs which source it used at
startup).

Once the discovery is completed you should be able to list your devices:

```
root@jaspy:/etc/prometheus# jaspy-devices
enabled  fqdn                             type                     software
default  main-sq.foobar.com               WS-C2960XR-24PD-I
```

You should now be able to see Prometheus metrics. The entity sensor and STP
metrics that used to have their own `jaspy-entitypoller` exporter on port 8098
are now served by `jaspy-nexus` itself on the same `/dev/metrics` endpoint as
the interface counters. Try:

```
curl http://localhost:8000/dev/metrics |grep jaspy

jaspy_stp_port_enabled{fqdn="main-sq.foobar.com",hostname="main-sq",interface_id="10105",interface_name="GigabitEthernet1/0/5",stp_port_id="5",vlan="1"} 1
jaspy_stp_port_enabled{fqdn="main-sq.foobar.com",hostname="main-sq",interface_id="10105",interface_name="GigabitEthernet1/0/5",stp_port_id="5",vlan="192"} 1
jaspy_stp_port_enabled{fqdn="main-sq.foobar.com",hostname="main-sq",interface_id="10105",interface_name="GigabitEthernet1/0/5",stp_port_id="5",vlan="2"} 1
jaspy_stp_port_enabled{fqdn="main-sq.foobar.com",hostname="main-sq",interface_id="10105",interface_name="GigabitEthernet1/0/5",stp_port_id="5",vlan="20"} 1
```

### Configure Prometheus

Edit `/var/prometheus/prometheus.yaml` and add this snippet to the bottom so that it becomes part of the `scrape_configs:`. This is required so that your Prometheus will poll Jaspy for network metrics. There are two sections: fast metrics and slow metrics (the slow endpoint now also carries the entity sensor and STP metrics that the standalone `jaspy-entitypoller` used to expose on port 8098).

```
  # notice indentation of two spaces
  - job_name: 'jaspy-poller-fast'
    scrape_interval: 10s
    metrics_path: '/dev/metrics/fast'
    static_configs:
      - targets: ['127.0.0.1:8000']

  - job_name: 'jaspy-poller-slow'
    scrape_interval: 30s
    metrics_path: '/dev/metrics'
    static_configs:
      - targets: ['127.0.0.1:8000']
```

Run `systemctl restart prometheus` to reload changes.

Open Prometheus (http://localhost:9090) and try these queries:

* `jaspy_device_up` Shows device status (value 1 if up, 0 if down)
* `jaspy_interface_broadcast_packets` Shows number of broadcast packets.
* `jaspy_interface_discards` Shows number of discarded packets
* `jaspy_interface_errors` Shows number of errors
* `jaspy_interface_multicast_packets` Shows number of multicast packets
* `jaspy_interface_octets` Shows number of bytes sent/received
* `jaspy_interface_speed` Shows line speed in Mbps
* `jaspy_interface_unicast_packets` Shows number of unicast packets sent/received
* `jaspy_interface_up` Shows 1 if the interface is up, 0 otherwise.
* `jaspy_interface_sensors` Shows various device/chassis sensors.
* `jaspy_stp_port_designated_cost`
* `jaspy_stp_port_enabled`
* `jaspy_stp_port_forward_transitions`
* `jaspy_stp_port_path_cost`
* `jaspy_stp_port_priority`
* `jaspy_stp_port_role`
* `jaspy_stp_port_state`

The metrics contains several Prometheus labels:
* `fqdn`
* `hostname`
* `interface_type`
* `name`
* `direction` ("tx" or "rx")
* `neighbors`
* `interface_id`
* `interface_name`

In addition the `jaspy_sensors` metrics contains these labels:
* `sensor_description`
* `sensor_id`
* `sensor_name`
* `value_type` (eg. "amperes", "celcius", "dBm"...)

## SNMP trap integration (instant link up/down)

The SNMP poller samples interfaces every ~10 seconds; for instant link state
changes, configure your switches to send linkUp/linkDown traps to the jaspy
VM and let snmptrapd hand them to nexus. The trap handler is a subcommand of
the `jaspy-nexus` binary (formerly the separate `jaspy-snmptrapd-reader`).

Install snmptrapd (`apt-get install snmptrapd`) with IF-MIB available, and
use a minimal `/etc/snmp/snmptrapd.conf`:

```
authCommunity log,execute,net somecommunityhere
traphandle IF-MIB::linkUp JASPY_URL=http://127.0.0.1:8000 /usr/lib/jaspy/jaspy-nexus trap-handler
traphandle IF-MIB::linkDown JASPY_URL=http://127.0.0.1:8000 /usr/lib/jaspy/jaspy-nexus trap-handler
```

The handler reports over the local nexus API (`PUT /dev/interface/monitor`),
which updates the live interface state immediately and publishes an
`interfaceUpDown` MQTT event on state changes.

## SNMP modes: external snmpbot vs. embedded client

nexus talks to devices over SNMP in one of two modes, selected by
`JASPY_SNMP_MODE` (default `snmpbot`):

* `snmpbot` (default): the external [qmsk/snmpbot](https://github.com/qmsk/snmpbot)
  HTTP sidecar does the SNMP I/O. This is the historical setup and needs the
  separate `snmpbot` deb + service (`JASPY_SNMPBOT_URL`, default
  `http://127.0.0.1:8286/`).
* `embedded`: nexus polls devices itself with an in-process SNMP v2c client and
  a built-in SNMP trap receiver — no snmpbot and no snmptrapd. It reads the same
  MIB JSON files snmpbot uses (shipped in the jaspy package at
  `/usr/share/jaspy/mibs`), so the request set and value encodings are identical.

To switch an installation to embedded mode, set on `jaspy-nexus`:

```
JASPY_SNMP_MODE=embedded
JASPY_SNMP_MIB_DIR=/usr/share/jaspy/mibs   # default; override if relocated
JASPY_ENABLE_TRAP_RECEIVER=true            # replaces snmptrapd (see above)
JASPY_TRAP_BIND_ADDRESS=0.0.0.0:162        # needs CAP_NET_BIND_SERVICE
```

then stop and disable `snmpbot` (and `snmptrapd` if the embedded trap receiver
is enabled). Binding UDP/162 requires `CAP_NET_BIND_SERVICE`, already granted in
the shipped `jaspy-nexus.service`; container deployments must publish UDP/162
and add the capability. Tuning knobs (all optional): `JASPY_SNMP_TIMEOUT_MS`
(default 2000), `JASPY_SNMP_RETRIES` (2), `JASPY_SNMP_BULK_MAX_REPETITIONS`
(20). Only SNMP v2c (community strings) is supported, matching snmpbot; the
active mode and loaded MIB count are reported by `GET /api/v1/system`.

## Weathermap

Weathermap is a JavaScript tool to show a graphical representation of the network topology in browser. It requires access to Prometheus and jaspy-api.

`jaspy-nexus` serves it directly at `http://<jaspy-vm>:8000/weathermap/` from
`/var/lib/jaspy/weathermap` (no apache needed). To configure it, copy
`/var/lib/jaspy/weathermap/js/config.dist.js` as `config.js` and replace
JASPY_PROMETHEUS_URL and JASPY_NEXUS_URL. Because nexus serves both the files
and the API, the nexus URL can be relative:

```
config = {
    "prometheusQueryURL": "http://172.16.143.39:9090/api/v1/query?query=",
    "jaspyNexusURL": "/dev/weathermap",
    "deviceIconSize": 20,
    "arrowWidth": 8.0,
    "arrowLength": 16.0,
    "linkGroupWidth": 8.0,
    "springDistance": 64.0,
    "speedFactorFactor": 0.1,
};
```

(The prometheus URL must still point at your Prometheus instance; expose it to
the browsers you use the weathermap from.)

