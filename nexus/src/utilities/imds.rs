use crate::models;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc,Mutex};
use crate::utilities;
use crate::utilities::health::{HealthStore, HealthConfig, SampleInput};
use crate::db::AnyConnection;

pub struct IMDS {
    metrics_storage : models::metrics::Metrics,
    msgbus: Arc<Mutex<utilities::msgbus::MessageBus>>,
    // Recent per-interface health signals (flapping, discards, errors, speed
    // renegotiation, utilization). Fed from report_interfaces below and read by
    // the /api/v1 device routes; see utilities::health.
    health: HealthStore,
}

// new - old for a monotonic counter pair; 0 when either side is absent or the
// counter went backwards (a reset — validate_counters already guards these).
fn counter_delta(old: &Option<u64>, new: &Option<u64>) -> u64 {
    match (old, new) {
        (Some(old), Some(new)) => new.saturating_sub(*old),
        _ => 0,
    }
}

struct ConnectionPair {
    local_device: models::dbo::Device,
    remote_info: Option<ConnectionPairRemoteInfo>,
}

struct ConnectionPairRemoteInfo {
    device: models::dbo::Device,
    interface: models::dbo::Interface,
}

impl ConnectionPair {
    fn load_by_fqdn_ifindex(connection: &mut AnyConnection, fqdn: &String, ifindex: &i32) -> Option<ConnectionPair> {
        if let Some(local_device) = models::dbo::Device::find_by_fqdn(connection, fqdn) {
            if let Some(local_interface) = local_device.interface_by_index(connection, ifindex) {
                let connpair : ConnectionPair;
                if let Some(remote_interface) = local_interface.peer_interface(connection) {
                    let remote_device = remote_interface.device(connection);
                    connpair = ConnectionPair {
                        local_device: local_device,
                        remote_info: Some(ConnectionPairRemoteInfo {
                            device: remote_device,
                            interface: remote_interface,
                        }),
                    };
                } else {
                    connpair = ConnectionPair {
                        local_device: local_device,
                        remote_info: None,
                    };
                }

                return Some(connpair);
            }
        }
        return None;
    }
}

impl IMDS {
    pub fn new(msgbus: Arc<Mutex<utilities::msgbus::MessageBus>>, health_cfg: HealthConfig) -> IMDS {
        let imds = IMDS {
            metrics_storage: models::metrics::Metrics {
                devices: HashMap::new()
            },
            msgbus: msgbus,
            health: HealthStore::new(health_cfg),
        };

        return imds;
    }

    pub fn get_device(self: &IMDS, device_fqdn: &str) -> Option<&models::metrics::DeviceMetrics> {
        return self.metrics_storage.devices.get(device_fqdn);
    }

    // Drop devices that no longer exist in the database (deleted via the API
    // or a state reset); without this their metrics would be exported forever.
    pub fn retain_devices(self: &mut IMDS, monitored_fqdns: &HashSet<String>) {
        self.metrics_storage.devices.retain(|fqdn, _| monitored_fqdns.contains(fqdn));
        self.health.retain_devices(monitored_fqdns);
    }

    // Drop interfaces from a device that no longer exist in the DB. Discovery
    // keys interfaces by name and rewrites the ifindex in place when it changes
    // (module reseat, reboot, device swap); the IMDS refresh loop keys on ifindex
    // and only ever inserts/updates, so a changed-or-removed ifindex would
    // otherwise orphan the old entry forever. Since ifindex is not a metric
    // label, such a ghost exports a duplicate series frozen at stale values.
    // Mirrors retain_devices, one level down. No-op for unknown devices.
    pub fn retain_interfaces(self: &mut IMDS, device_fqdn: &String, live_ifindexes: &HashSet<i32>) {
        if let Some(device) = self.metrics_storage.devices.get_mut(device_fqdn) {
            device.interfaces.retain(|ifindex, _| live_ifindexes.contains(ifindex));
        }
        self.health.retain_interfaces(device_fqdn, live_ifindexes);
    }

    // Health summary for one interface (None when healthy). The interface's
    // last-poll timestamp drives the stale check.
    pub fn interface_health(self: &IMDS, fqdn: &str, ifindex: i32, now: u64) -> Option<crate::utilities::health::InterfaceHealthSummary> {
        let last_report = self.metrics_storage.devices.get(fqdn)?.interfaces.get(&ifindex)?.last_report;
        self.health.summary(fqdn, ifindex, now, last_report)
    }

    // Worst interface health severity for a device, for the /devices list badge.
    pub fn device_health(self: &IMDS, fqdn: &str, now: u64) -> Option<crate::utilities::health::Severity> {
        let device = self.metrics_storage.devices.get(fqdn)?;
        let last_reports: HashMap<i32, u64> = device.interfaces.iter().map(|(ifindex, iface)| (*ifindex, iface.last_report)).collect();
        self.health.device_rollup(fqdn, now, &last_reports)
    }

    // Optional disk persistence of the health store (see main.rs).
    pub fn health_to_json(self: &IMDS) -> Result<String, serde_json::Error> {
        self.health.to_json()
    }

    pub fn load_health_json(self: &mut IMDS, json: &str) -> Result<(), serde_json::Error> {
        self.health.load_json(json)
    }

    pub fn refresh_device(self: &mut IMDS, device_fqdn: &String, base_mac: &Option<String>) {
        let mut existed = false;
        let mut hardware_swapped = false;
        if let Some(device) = self.metrics_storage.devices.get_mut(device_fqdn) {
            existed = true;
            device.last_report = utilities::tools::get_time_msecs();
            // A changed chassis base MAC means the physical device was replaced
            // behind the same fqdn/ip. Reset every interface's accumulated
            // counters + operational state so the new hardware's fresh (lower)
            // counters aren't rejected as regressions by validate_counters, and
            // no stale samples linger. The stored value is normalized (discovery
            // emits colon- or space-separated forms and the field is
            // operator-editable), so only a genuine MAC change trips the reset,
            // not a reformatting. Only a KNOWN, non-empty MAC updates state: a
            // missing MAC (None) leaves the last-known value intact, so a
            // transient gap in discovery neither discards it nor masks a later
            // real change. Learning a MAC for the first time is not a swap.
            let new_normalized = base_mac.as_deref()
                .map(crate::utilities::stp::normalize_mac)
                .filter(|m| !m.is_empty());
            if let Some(new_mac) = new_normalized {
                match &device.base_mac {
                    Some(old_mac) if *old_mac == new_mac => {}, // unchanged: no write
                    Some(_) => {
                        hardware_swapped = true;
                        for interface in device.interfaces.values_mut() {
                            interface.reset_counters();
                        }
                        device.base_mac = Some(new_mac);
                    },
                    None => { device.base_mac = Some(new_mac); }, // first learn
                }
            }
        }
        if !existed {
            let fqdn_splitted : Vec<&str> = device_fqdn.split('.').collect();
            let hostname = fqdn_splitted[0];
            let dm = models::metrics::DeviceMetrics {
                last_report: 0,
                last_poll: 0,
                fqdn: device_fqdn.clone(),
                hostname: hostname.to_string(),
                base_mac: base_mac.as_deref()
                    .map(crate::utilities::stp::normalize_mac)
                    .filter(|m| !m.is_empty()),
                up: None,
                interfaces: HashMap::new(),
            };
            self.metrics_storage.devices.insert(device_fqdn.clone(), dm);
        }
        // Health history belongs to the removed hardware — drop it too. Done
        // after the device borrow ends to satisfy the borrow checker.
        if hardware_swapped {
            self.health.forget_device_interfaces(device_fqdn);
        }
    }

    pub fn report_device(self: &mut IMDS, connection: &mut AnyConnection, dmr: models::json::DeviceMonitorReport) {
        // Upsert: the pinger only reports monitored devices, but at startup it
        // can ping (and report) before the DB refresh / poller has populated
        // IMDS. Creating the entry on demand keeps an early up/down report from
        // being dropped — otherwise a reachable device whose first ping reply
        // beats the first IMDS refresh would stay "unknown" (up=None) forever,
        // since the pinger only re-reports on state transitions. Mirrors the
        // minimal entry refresh_device seeds; later refreshes fill base_mac etc.
        let now = utilities::tools::get_time_msecs();
        let hostname = dmr.fqdn.split('.').next().unwrap_or(&dmr.fqdn).to_string();
        let device = self.metrics_storage.devices
            .entry(dmr.fqdn.clone())
            .or_insert_with(|| models::metrics::DeviceMetrics {
                last_report: 0,
                last_poll: 0,
                fqdn: dmr.fqdn.clone(),
                hostname,
                base_mac: None,
                up: None,
                interfaces: HashMap::new(),
            });
        device.last_report = now;


        if let Some(device_up) = device.up {
            if device_up != dmr.up {
                if let Some(device) = models::dbo::Device::find_by_fqdn(connection, &dmr.fqdn) {
                    let mut neighbors: HashSet<String> = HashSet::new();
                    for interface in device.interfaces(connection).iter() {
                        if let Some(conn_iface) = interface.peer_interface(connection) {
                            let conn_device = conn_iface.device(connection);
                            let conn_fqdn = format!("{}.{}", conn_device.name, conn_device.dns_domain);
                            if !neighbors.contains(&conn_fqdn) {
                                neighbors.insert(conn_fqdn);
                            }
                        }
                    }
                    if let Ok(ref mut msgbus) = self.msgbus.lock() {
                        let event = models::events::Event::ping_change_event(&dmr.fqdn, neighbors, device_up, dmr.up);
                        msgbus.event(event);
                    }
                }
            }
        }
        device.up = Some(dmr.up);
    }

    pub fn refresh_interface(self: &mut IMDS, device_fqdn: &String, if_index: i32, interface_type: &String, name: &String, neighbors: bool, speed_override: Option<i32>) {
        let device;
        match self.metrics_storage.devices.get_mut(device_fqdn) {
            Some(value) => {
                device = value;
            },
            None => {
                // TODO: log? wtf?
                return;
            }
        }
        match device.interfaces.get_mut(&if_index) {
            Some(target_interface) => {
                if target_interface.name != *name { target_interface.name = name.clone(); }
                // interface_type is deliberately NOT updated here: it is a
                // Prometheus label (see metrics_from), so mutating it in place
                // would end the interface's counter series and start a new one,
                // fabricating a rate() reset/gap. It is effectively immutable
                // after creation — ifType almost never changes on a live port,
                // and a port that genuinely changes gets a new ifindex (hence a
                // fresh entry). A chassis swap is handled by refresh_device.
                target_interface.neighbors = neighbors;
                target_interface.speed_override = speed_override;
                return;
            },
            None => {}
        }
        device.interfaces.insert(if_index, models::metrics::InterfaceMetrics {
            last_report: 0,
            name: name.clone(),
            neighbors: neighbors,
            speed_override: speed_override,
            interface_type: interface_type.clone(),

            in_octets: None,
            out_octets: None,
            in_unicast_packets: None,
            in_multicast_packets: None,
            in_broadcast_packets: None,
            out_unicast_packets: None,
            out_multicast_packets: None,
            out_broadcast_packets: None,
            in_errors: None,
            out_errors: None,
            out_discards: None,
            up: None,
            speed: None,

            counter_violations: 0,
        });
    }

    fn validate_u64_forward_progress(old: &Option<u64>, new: &Option<u64>) -> bool {
        if let Some(old) = old {
            if let Some(new) = new {
                if old <= new {
                    return true;
                }
                // Note, the value here is just an arbitrary number
                if (old - new) < 2147483647 {
                    return false;
                }
            }
        }
        return true;
    }

    fn validate_counters(current_value: &mut models::metrics::InterfaceMetrics, new_value: &models::json::InterfaceMonitorInterfaceReport) -> bool {
        if current_value.counter_violations >= 10 {
            current_value.counter_violations = 0;
            return true;
        }
        let mut success: bool = true;
        success = success && IMDS::validate_u64_forward_progress(&current_value.in_octets, &new_value.in_octets);
        success = success && IMDS::validate_u64_forward_progress(&current_value.out_octets, &new_value.out_octets);
        success = success && IMDS::validate_u64_forward_progress(&current_value.in_unicast_packets, &new_value.in_unicast_packets);
        success = success && IMDS::validate_u64_forward_progress(&current_value.in_multicast_packets, &new_value.in_multicast_packets);
        success = success && IMDS::validate_u64_forward_progress(&current_value.in_broadcast_packets, &new_value.in_broadcast_packets);
        success = success && IMDS::validate_u64_forward_progress(&current_value.out_unicast_packets, &new_value.out_unicast_packets);
        success = success && IMDS::validate_u64_forward_progress(&current_value.out_multicast_packets, &new_value.out_multicast_packets);
        success = success && IMDS::validate_u64_forward_progress(&current_value.out_broadcast_packets, &new_value.out_broadcast_packets);
        success = success && IMDS::validate_u64_forward_progress(&current_value.in_errors, &new_value.in_errors);
        success = success && IMDS::validate_u64_forward_progress(&current_value.out_errors, &new_value.out_errors);
        success = success && IMDS::validate_u64_forward_progress(&current_value.out_discards, &new_value.out_discards);
        if !success {
            current_value.counter_violations += 1;
            return false;
        } else {
            current_value.counter_violations = 0;
            return true;
        }
    }

    pub fn report_interfaces(self: &mut IMDS, connection: &mut AnyConnection, imr: models::json::InterfaceMonitorReport) {
        let device;
        let last_report = utilities::tools::get_time_msecs();
        match self.metrics_storage.devices.get_mut(&imr.device_fqdn) {
            Some(value) => {
                device = value;
                device.last_poll = last_report;
            },
            None => {
                // TODO: log? this means we got a report from a host that is not being monitored, it is possible this is normal on device removal
                return;
            }
        }
        // Snapshot the device's interfaces ONCE per report, not once per
        // interface. This is only read to annotate link-flap events with LAG
        // peer statuses (below); cloning it inside the loop made report cost
        // O(interfaces^2) under the global IMDS lock (PERF.md #1). One clone per
        // device keeps report_interfaces O(interfaces).
        let interfaces_snapshot = device.interfaces.clone();
        for interface_report in imr.interfaces.iter() {
            let interface;
            match device.interfaces.get_mut(&interface_report.if_index) {
                Some(target_interface) => { interface = target_interface; },
                None => {
                    // TODO: log? this means we got a report for interface we don't really follow
                    continue;
                }
            }

            if interface.last_report >= last_report {
                // TODO: log this case
                continue;
            }

            if !IMDS::validate_counters(interface, &interface_report) {
                // TODO: log this case
                continue;
            }

            // Capture pre-update values for the health store: the counters
            // below are overwritten in place, so deltas must be computed first.
            let prev_last_report = interface.last_report;
            let health_interval_ms = if prev_last_report > 0 { last_report.saturating_sub(prev_last_report) } else { 0 };
            let d_in_errors = counter_delta(&interface.in_errors, &interface_report.in_errors);
            let d_out_errors = counter_delta(&interface.out_errors, &interface_report.out_errors);
            let d_out_discards = counter_delta(&interface.out_discards, &interface_report.out_discards);
            let d_in_octets = counter_delta(&interface.in_octets, &interface_report.in_octets);
            let d_out_octets = counter_delta(&interface.out_octets, &interface_report.out_octets);
            let mut up_transition: Option<bool> = None;
            let mut speed_change: Option<(Option<i32>, i32)> = None;

            interface.last_report = last_report;
            // TODO: statechanges should be emitted for errors?
            if interface_report.in_octets.is_some() { interface.in_octets = interface_report.in_octets; }
            if interface_report.out_octets.is_some() { interface.out_octets = interface_report.out_octets; }
            if interface_report.in_unicast_packets.is_some() { interface.in_unicast_packets = interface_report.in_unicast_packets; }
            if interface_report.in_multicast_packets.is_some() { interface.in_multicast_packets = interface_report.in_multicast_packets; }
            if interface_report.in_broadcast_packets.is_some() { interface.in_broadcast_packets = interface_report.in_broadcast_packets; }
            if interface_report.out_unicast_packets.is_some() { interface.out_unicast_packets = interface_report.out_unicast_packets; }
            if interface_report.out_multicast_packets.is_some() { interface.out_multicast_packets = interface_report.out_multicast_packets; }
            if interface_report.out_broadcast_packets.is_some() { interface.out_broadcast_packets = interface_report.out_broadcast_packets; }
            if interface_report.in_errors.is_some() { interface.in_errors = interface_report.in_errors; }
            if interface_report.out_errors.is_some() { interface.out_errors = interface_report.out_errors; }
            if interface_report.out_discards.is_some() { interface.out_discards = interface_report.out_discards; }
            if interface_report.up.is_some() {
                // TODO: fix this nested hellhole :)
                if let Some(old_state) = interface.up {
                    if let Some(new_state) = interface_report.up {
                        if old_state != new_state {
                            up_transition = Some(new_state);
                            let mut neighbor : Option<String> = None;
                            let mut neighbor_interface_name : Option<String> = None;
                            let mut link_interfaces : Vec<models::dbo::Interface> = Vec::new();
                            let mut link_statuses : HashMap<String, String> = HashMap::new();
                            if let Some(connpair) = ConnectionPair::load_by_fqdn_ifindex(connection, &imr.device_fqdn, &interface_report.if_index) {
                                if let Some(remote_info) = connpair.remote_info {
                                    neighbor = Some(format!("{}.{}", remote_info.device.name, remote_info.device.dns_domain));
                                    neighbor_interface_name = Some(remote_info.interface.name());
                                    for remote_peer_candidate in remote_info.device.interfaces(connection) {
                                        if let Some(rpc_remote_interface) = remote_peer_candidate.peer_interface(connection) {
                                            if rpc_remote_interface.device_id == connpair.local_device.id {
                                                link_interfaces.push(rpc_remote_interface.clone());
                                            }
                                        }
                                    }
                                }
                            }
                            for link_interface in link_interfaces.iter() {
                                if let Some(link_interface_data) = interfaces_snapshot.get(&link_interface.index) {
                                    // The interface that just flapped reports its
                                    // fresh state; its LAG peers keep their
                                    // last-known state from the snapshot.
                                    let up = if link_interface.index == interface_report.if_index {
                                        interface_report.up
                                    } else {
                                        link_interface_data.up
                                    };
                                    let status = match up {
                                        Some(true) => "up".to_string(),
                                        Some(false) => "down".to_string(),
                                        None => "unknown".to_string(),
                                    };
                                    link_statuses.insert(link_interface_data.name.clone(), status);
                                }
                            }
                            if let Ok(ref mut msgbus) = self.msgbus.lock() {
                                let event = models::events::Event::interface_updown_event(&imr.device_fqdn, &interface.name, neighbor, neighbor_interface_name, &link_statuses, old_state, new_state);
                                msgbus.event(event);
                            }
                        }
                    }
                }
                interface.up = interface_report.up;
            }
            if interface_report.speed.is_some() {
                if let Some(old_state) = interface.speed {
                    if let Some(new_state) = interface_report.speed {
                        if old_state != new_state {
                            speed_change = Some((Some(old_state), new_state));
                            let mut neighbor : Option<String> = None;
                            let mut neighbor_interface_name : Option<String> = None;
                            if let Some(connpair) = ConnectionPair::load_by_fqdn_ifindex(connection, &imr.device_fqdn, &interface_report.if_index) {
                                if let Some(remote_info) = connpair.remote_info {
                                    neighbor = Some(format!("{}.{}", remote_info.device.name, remote_info.device.dns_domain));
                                    neighbor_interface_name = Some(remote_info.interface.name());
                                }
                            }
                            if let Ok(ref mut msgbus) = self.msgbus.lock() {
                                let event = models::events::Event::interface_speed_event(&imr.device_fqdn, &interface.name, neighbor, neighbor_interface_name, old_state, new_state);
                                msgbus.event(event);
                            }
                        }
                    }
                }
                interface.speed = interface_report.speed;
            }

            // Feed the recent-history health store. Effective speed prefers the
            // manual override (matches get_metrics and the API's reported speed).
            let effective_speed = interface.speed_override.or(interface.speed);
            self.health.ingest(
                &imr.device_fqdn,
                interface_report.if_index,
                SampleInput {
                    in_errors: d_in_errors,
                    out_errors: d_out_errors,
                    out_discards: d_out_discards,
                    in_octets: d_in_octets,
                    out_octets: d_out_octets,
                    interval_ms: health_interval_ms,
                    up_transition,
                    speed_change,
                    speed_mbps: effective_speed,
                },
                last_report,
            );
        }
    }

    // Convenience wrapper used by the unit tests. Non-test callers (the metrics
    // route) go through metrics_snapshot + metrics_from so the build runs
    // outside the lock (PERF.md #2).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn get_metrics(self: &IMDS) -> Vec<models::metrics::LabeledMetric> {
        IMDS::metrics_from(&self.metrics_storage)
    }

    // Cheap owned snapshot of the metric store. Taken under the IMDS lock so the
    // expensive LabeledMetric build (metrics_from) can run OUTSIDE the lock and
    // stop blocking every poller for the duration of a scrape (PERF.md #2).
    pub fn metrics_snapshot(self: &IMDS) -> models::metrics::Metrics {
        self.metrics_storage.clone()
    }

    // Build the Prometheus metric list from a (snapshotted) store. Pure — holds
    // no lock — so it is safe to call after releasing the IMDS mutex.
    pub fn metrics_from(storage: &models::metrics::Metrics) -> Vec<models::metrics::LabeledMetric> {
        let jaspy_interface_octets = "jaspy_interface_octets".to_string();
        let jaspy_interface_unicast_packets = "jaspy_interface_unicast_packets".to_string();
        let jaspy_interface_multicast_packets = "jaspy_interface_multicast_packets".to_string();
        let jaspy_interface_broadcast_packets = "jaspy_interface_broadcast_packets".to_string();
        let jaspy_interface_errors = "jaspy_interface_errors".to_string();
        let jaspy_interface_speed = "jaspy_interface_speed".to_string();
        let jaspy_interface_discards = "jaspy_interface_discards".to_string();

        let mut metric_values: Vec<models::metrics::LabeledMetric> = Vec::new();
        for (_device_key, device_metrics) in storage.devices.iter() {
            for (_interface_key, interface_metrics) in device_metrics.interfaces.iter() {
                let reported_speed = match interface_metrics.speed_override {
                    Some(speed_override) => Some(speed_override),
                    None => interface_metrics.speed
                };

                let mut labels: HashMap<String,String> = HashMap::new();
                labels.insert("fqdn".to_string(), device_metrics.fqdn.clone());
                labels.insert("hostname".to_string(), device_metrics.hostname.clone());
                labels.insert("name".to_string(), interface_metrics.name.clone());
                labels.insert("interface_type".to_string(), interface_metrics.interface_type.clone());
                if interface_metrics.neighbors { labels.insert("neighbors".to_string(), "yes".to_string()); }
                else { labels.insert("neighbors".to_string(), "no".to_string()); }

                let mut in_labels = labels.clone();
                in_labels.insert("direction".to_string(), "rx".to_string());
                let mut out_labels = labels.clone();
                out_labels.insert("direction".to_string(), "tx".to_string());

                if let Some(interface_metrics_reported_speed) = reported_speed {
                    metric_values.push(models::metrics::LabeledMetric::new(
                        &jaspy_interface_speed, models::metrics::MetricValue::Int64(interface_metrics_reported_speed as i64),
                        &labels,
                        interface_metrics.last_report,
                    ));
                }

                if let Some(interface_metrics_in_octets) = interface_metrics.in_octets {
                    metric_values.push(models::metrics::LabeledMetric::new(
                        &jaspy_interface_octets, models::metrics::MetricValue::Uint64(interface_metrics_in_octets),
                        &in_labels,
                        interface_metrics.last_report,
                    ));
                }

                if let Some(interface_metrics_out_octets) = interface_metrics.out_octets {
                    metric_values.push(models::metrics::LabeledMetric::new(
                        &jaspy_interface_octets, models::metrics::MetricValue::Uint64(interface_metrics_out_octets),
                        &out_labels,
                        interface_metrics.last_report,
                    ));
                }

                if let Some(interface_metrics_in_unicast_packets) = interface_metrics.in_unicast_packets {
                    metric_values.push(models::metrics::LabeledMetric::new(
                        &jaspy_interface_unicast_packets, models::metrics::MetricValue::Uint64(interface_metrics_in_unicast_packets),
                        &in_labels,
                        interface_metrics.last_report,
                    ));
                }

                if let Some(interface_metrics_in_multicast_packets) = interface_metrics.in_multicast_packets {
                    metric_values.push(models::metrics::LabeledMetric::new(
                        &jaspy_interface_multicast_packets, models::metrics::MetricValue::Uint64(interface_metrics_in_multicast_packets),
                        &in_labels,
                        interface_metrics.last_report,
                    ));
                }

                if let Some(interface_metrics_in_broadcast_packets) = interface_metrics.in_broadcast_packets {
                    metric_values.push(models::metrics::LabeledMetric::new(
                        &jaspy_interface_broadcast_packets, models::metrics::MetricValue::Uint64(interface_metrics_in_broadcast_packets),
                        &in_labels,
                        interface_metrics.last_report,
                    ));
                }

                if let Some(interface_metrics_out_unicast_packets) = interface_metrics.out_unicast_packets {
                    metric_values.push(models::metrics::LabeledMetric::new(
                        &jaspy_interface_unicast_packets, models::metrics::MetricValue::Uint64(interface_metrics_out_unicast_packets),
                        &out_labels,
                        interface_metrics.last_report,
                    ));
                }

                if let Some(interface_metrics_out_multicast_packets) = interface_metrics.out_multicast_packets {
                    metric_values.push(models::metrics::LabeledMetric::new(
                        &jaspy_interface_multicast_packets, models::metrics::MetricValue::Uint64(interface_metrics_out_multicast_packets),
                        &out_labels,
                        interface_metrics.last_report,
                    ));
                }

                if let Some(interface_metrics_out_broadcast_packets) = interface_metrics.out_broadcast_packets {
                    metric_values.push(models::metrics::LabeledMetric::new(
                        &jaspy_interface_broadcast_packets, models::metrics::MetricValue::Uint64(interface_metrics_out_broadcast_packets),
                        &out_labels,
                        interface_metrics.last_report,
                    ));
                }

                if let Some(interface_metrics_in_errors) = interface_metrics.in_errors {
                    metric_values.push(models::metrics::LabeledMetric::new(
                        &jaspy_interface_errors, models::metrics::MetricValue::Uint64(interface_metrics_in_errors),
                        &in_labels,
                        interface_metrics.last_report,
                    ));
                }

                if let Some(interface_metrics_out_errors) = interface_metrics.out_errors {
                    metric_values.push(models::metrics::LabeledMetric::new(
                        &jaspy_interface_errors, models::metrics::MetricValue::Uint64(interface_metrics_out_errors),
                        &out_labels,
                        interface_metrics.last_report,
                    ));
                }

                if let Some(interface_metrics_out_discards) = interface_metrics.out_discards {
                    metric_values.push(models::metrics::LabeledMetric::new(
                        &jaspy_interface_discards, models::metrics::MetricValue::Uint64(interface_metrics_out_discards),
                        &out_labels,
                        interface_metrics.last_report,
                    ));
                }
            }
        }

        return metric_values;
    }

    #[cfg(test)]
    fn interface_mut(self: &mut IMDS, device_fqdn: &str, if_index: i32) -> &mut models::metrics::InterfaceMetrics {
        self.metrics_storage.devices.get_mut(device_fqdn).unwrap().interfaces.get_mut(&if_index).unwrap()
    }

    pub fn get_fast_metrics(self: &IMDS) -> Vec<models::metrics::LabeledMetric> {
        let jaspy_device_up = "jaspy_device_up".to_string();
        let jaspy_interface_up = "jaspy_interface_up".to_string();

        let mut metric_values: Vec<models::metrics::LabeledMetric> = Vec::new();
        for (_device_key, device_metrics) in self.metrics_storage.devices.iter() {
            // Only emit device up/down metrics if device is actually up/down aka. not indeterminate :)
            if let Some(device_up_bool) = device_metrics.up {
                let device_up : i64;
                let mut labels: HashMap<String,String> = HashMap::new();
                labels.insert("fqdn".to_string(), device_metrics.fqdn.clone());
                labels.insert("hostname".to_string(), device_metrics.hostname.clone());
                if device_up_bool { device_up = 1; } else { device_up = 0; }
                let metric = models::metrics::LabeledMetric::new(
                    &jaspy_device_up, models::metrics::MetricValue::Int64(device_up),
                    &labels,
                    device_metrics.last_report,
                );
                metric_values.push(metric);
            }

            for (_interface_key, interface_metrics) in device_metrics.interfaces.iter() {
                let mut labels: HashMap<String,String> = HashMap::new();
                labels.insert("fqdn".to_string(), device_metrics.fqdn.clone());
                labels.insert("name".to_string(), interface_metrics.name.clone());
                labels.insert("hostname".to_string(), device_metrics.hostname.clone());
                labels.insert("interface_type".to_string(), interface_metrics.interface_type.clone());
                if interface_metrics.neighbors { labels.insert("neighbors".to_string(), "yes".to_string()); }
                else { labels.insert("neighbors".to_string(), "no".to_string()); }

                if let Some(interface_metrics_up) = interface_metrics.up {
                    let val : i64;
                    if interface_metrics_up { val = 1; } else { val = 0; }
                    metric_values.push(models::metrics::LabeledMetric::new(
                        &jaspy_interface_up, models::metrics::MetricValue::Int64(val),
                        &labels,
                        interface_metrics.last_report,
                    ));
                }
            }
        }

        return metric_values;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::json::InterfaceMonitorInterfaceReport;
    use crate::models::metrics::{InterfaceMetrics, LabeledMetric, MetricValue};

    fn test_imds() -> IMDS {
        IMDS::new(Arc::new(Mutex::new(utilities::msgbus::MessageBus::disconnected())), HealthConfig::default())
    }

    fn empty_report(if_index: i32) -> InterfaceMonitorInterfaceReport {
        InterfaceMonitorInterfaceReport {
            if_index: if_index,
            in_octets: None,
            out_octets: None,
            in_unicast_packets: None,
            in_multicast_packets: None,
            in_broadcast_packets: None,
            out_unicast_packets: None,
            out_multicast_packets: None,
            out_broadcast_packets: None,
            in_errors: None,
            out_errors: None,
            out_discards: None,
            up: None,
            speed: None,
        }
    }

    fn empty_interface(name: &str) -> InterfaceMetrics {
        InterfaceMetrics {
            name: name.to_string(),
            neighbors: false,
            interface_type: "ethernetCsmacd".to_string(),
            last_report: 0,
            speed_override: None,
            in_octets: None,
            out_octets: None,
            in_unicast_packets: None,
            in_multicast_packets: None,
            in_broadcast_packets: None,
            out_unicast_packets: None,
            out_multicast_packets: None,
            out_broadcast_packets: None,
            in_errors: None,
            out_errors: None,
            out_discards: None,
            up: None,
            speed: None,
            counter_violations: 0,
        }
    }

    fn metrics_by_name<'a>(metrics: &'a [LabeledMetric], name: &str) -> Vec<&'a LabeledMetric> {
        metrics.iter().filter(|m| m.name == name).collect()
    }

    // --- counter validation ---

    #[test]
    fn forward_progress_accepts_missing_old_or_new() {
        assert!(IMDS::validate_u64_forward_progress(&None, &Some(5)));
        assert!(IMDS::validate_u64_forward_progress(&Some(5), &None));
        assert!(IMDS::validate_u64_forward_progress(&None, &None));
    }

    #[test]
    fn forward_progress_accepts_equal_and_increase() {
        assert!(IMDS::validate_u64_forward_progress(&Some(5), &Some(5)));
        assert!(IMDS::validate_u64_forward_progress(&Some(5), &Some(6)));
    }

    #[test]
    fn forward_progress_rejects_small_decrease() {
        assert!(!IMDS::validate_u64_forward_progress(&Some(1000), &Some(999)));
    }

    #[test]
    fn forward_progress_accepts_wrap_sized_decrease() {
        // A drop of >= 2^31-1 is treated as a counter wrap, not a regression.
        assert!(IMDS::validate_u64_forward_progress(&Some(5_000_000_000), &Some(100)));
    }

    #[test]
    fn validate_counters_rejects_regression_and_counts_violation() {
        let mut current = empty_interface("Ethernet1/1");
        current.in_octets = Some(1000);
        let mut report = empty_report(1);
        report.in_octets = Some(999);
        assert!(!IMDS::validate_counters(&mut current, &report));
        assert_eq!(current.counter_violations, 1);
        // Stored value is untouched on rejection (report_interfaces skips the update).
        assert_eq!(current.in_octets, Some(1000));
    }

    #[test]
    fn validate_counters_accepts_after_ten_violations() {
        let mut current = empty_interface("Ethernet1/1");
        current.in_octets = Some(1000);
        current.counter_violations = 10;
        let mut report = empty_report(1);
        report.in_octets = Some(999);
        // Persistent "violations" mean the counters really did reset (e.g.
        // device reboot); accept and start over.
        assert!(IMDS::validate_counters(&mut current, &report));
        assert_eq!(current.counter_violations, 0);
    }

    #[test]
    fn validate_counters_resets_violations_on_success() {
        let mut current = empty_interface("Ethernet1/1");
        current.in_octets = Some(1000);
        current.counter_violations = 3;
        let mut report = empty_report(1);
        report.in_octets = Some(2000);
        assert!(IMDS::validate_counters(&mut current, &report));
        assert_eq!(current.counter_violations, 0);
    }

    // --- device/interface state ---

    #[test]
    fn refresh_device_creates_then_touches() {
        let mut imds = test_imds();
        let fqdn = "sw1.example.com".to_string();
        imds.refresh_device(&fqdn, &None);
        {
            let device = imds.get_device(&fqdn).unwrap();
            assert_eq!(device.hostname, "sw1");
            assert_eq!(device.fqdn, fqdn);
            assert_eq!(device.up, None);
            assert_eq!(device.last_report, 0);
        }
        imds.refresh_device(&fqdn, &None);
        assert!(imds.get_device(&fqdn).unwrap().last_report > 0);
    }

    #[test]
    fn retain_devices_drops_missing() {
        let mut imds = test_imds();
        imds.refresh_device(&"keep.example.com".to_string(), &None);
        imds.refresh_device(&"drop.example.com".to_string(), &None);
        let keep: HashSet<String> = vec!["keep.example.com".to_string()].into_iter().collect();
        imds.retain_devices(&keep);
        assert!(imds.get_device("keep.example.com").is_some());
        assert!(imds.get_device("drop.example.com").is_none());
    }

    #[test]
    fn refresh_interface_creates_then_updates() {
        let mut imds = test_imds();
        let fqdn = "sw1.example.com".to_string();
        imds.refresh_device(&fqdn, &None);
        imds.refresh_interface(&fqdn, 1, &"ethernetCsmacd".to_string(), &"Eth1".to_string(), false, None);
        {
            let iface = &imds.get_device(&fqdn).unwrap().interfaces[&1];
            assert_eq!(iface.name, "Eth1");
            assert_eq!(iface.neighbors, false);
            assert_eq!(iface.speed_override, None);
        }
        imds.refresh_interface(&fqdn, 1, &"ethernetCsmacd".to_string(), &"Ethernet1/1".to_string(), true, Some(10000));
        let device = imds.get_device(&fqdn).unwrap();
        assert_eq!(device.interfaces.len(), 1);
        let iface = &device.interfaces[&1];
        assert_eq!(iface.name, "Ethernet1/1");
        assert_eq!(iface.neighbors, true);
        assert_eq!(iface.speed_override, Some(10000));
    }

    #[test]
    fn refresh_interface_without_device_is_ignored() {
        let mut imds = test_imds();
        imds.refresh_interface(&"ghost.example.com".to_string(), 1, &"ethernetCsmacd".to_string(), &"Eth1".to_string(), false, None);
        assert!(imds.get_device("ghost.example.com").is_none());
    }

    // Reproduces the ghost-interface leak: discovery keys interfaces by name and
    // rewrites the ifindex in place when it changes (utilities/discovery.rs), but
    // the IMDS refresh loop keys on ifindex and only ever inserts/updates. When an
    // interface's ifindex changes (module reseat, reboot, device swap) the old
    // ifindex entry is orphaned. Because ifindex is NOT a metric label, the ghost
    // and the live interface export IDENTICAL label sets -> a duplicate Prometheus
    // series frozen at stale values.
    #[test]
    fn reindexed_interface_leaves_ghost_and_duplicate_metric() {
        let mut imds = test_imds();
        let fqdn = "sw1.example.com".to_string();
        imds.refresh_device(&fqdn, &None);

        // Interface "Gi1/0/1" first seen at ifindex 5, with a byte counter so it
        // renders a metric.
        imds.refresh_interface(&fqdn, 5, &"ethernetCsmacd".to_string(), &"Gi1/0/1".to_string(), false, None);
        imds.interface_mut(&fqdn, 5).in_octets = Some(100);

        // Same physical interface reappears at ifindex 7 (ifindex changed). The DB
        // row updated in place; the IMDS refresh calls refresh_interface for the
        // new ifindex.
        imds.refresh_interface(&fqdn, 7, &"ethernetCsmacd".to_string(), &"Gi1/0/1".to_string(), false, None);
        imds.interface_mut(&fqdn, 7).in_octets = Some(200);

        // BUG: both ifindexes now live in the map.
        assert_eq!(imds.get_device(&fqdn).unwrap().interfaces.len(), 2);

        // BUG: two rx-octet series for the SAME label set (name=Gi1/0/1) — a
        // duplicate Prometheus series.
        let metrics = imds.get_metrics();
        let dup: Vec<&LabeledMetric> = metrics
            .iter()
            .filter(|m| m.name == "jaspy_interface_octets"
                && m.labels.get("name").map(|n| n == "Gi1/0/1").unwrap_or(false)
                && m.labels.get("direction").map(|d| d == "rx").unwrap_or(false))
            .collect();
        assert_eq!(dup.len(), 2, "reindex should leave a duplicate rx series (the bug)");
    }

    // retain_interfaces reconciles the IMDS interface map against the DB's live
    // ifindex set, clearing the ghost from a reindex and its duplicate series.
    #[test]
    fn retain_interfaces_drops_reindexed_ghost() {
        let mut imds = test_imds();
        let fqdn = "sw1.example.com".to_string();
        imds.refresh_device(&fqdn, &None);
        imds.refresh_interface(&fqdn, 5, &"ethernetCsmacd".to_string(), &"Gi1/0/1".to_string(), false, None);
        imds.interface_mut(&fqdn, 5).in_octets = Some(100);
        imds.refresh_interface(&fqdn, 7, &"ethernetCsmacd".to_string(), &"Gi1/0/1".to_string(), false, None);
        imds.interface_mut(&fqdn, 7).in_octets = Some(200);

        // DB now only knows ifindex 7 (discovery rewrote the index in place).
        let live: HashSet<i32> = vec![7].into_iter().collect();
        imds.retain_interfaces(&fqdn, &live);

        let device = imds.get_device(&fqdn).unwrap();
        assert_eq!(device.interfaces.len(), 1);
        assert!(device.interfaces.contains_key(&7));
        assert!(!device.interfaces.contains_key(&5), "ghost ifindex must be gone");

        // No more duplicate series: exactly one rx-octet series for Gi1/0/1.
        let metrics = imds.get_metrics();
        let dup: Vec<&LabeledMetric> = metrics
            .iter()
            .filter(|m| m.name == "jaspy_interface_octets"
                && m.labels.get("name").map(|n| n == "Gi1/0/1").unwrap_or(false)
                && m.labels.get("direction").map(|d| d == "rx").unwrap_or(false))
            .collect();
        assert_eq!(dup.len(), 1);
        assert!(matches!(dup[0].value, MetricValue::Uint64(200)), "surviving series is the live ifindex 7");
    }

    // Removing an interface entirely (no same-name replacement) is also cleaned
    // up — the name-match-only heuristic from the report would miss this case.
    #[test]
    fn retain_interfaces_drops_removed_interface() {
        let mut imds = test_imds();
        let fqdn = "sw1.example.com".to_string();
        imds.refresh_device(&fqdn, &None);
        imds.refresh_interface(&fqdn, 1, &"ethernetCsmacd".to_string(), &"Gi1/0/1".to_string(), false, None);
        imds.refresh_interface(&fqdn, 2, &"ethernetCsmacd".to_string(), &"Gi1/0/2".to_string(), false, None);

        // DB dropped Gi1/0/2 (module removed); only ifindex 1 survives.
        let live: HashSet<i32> = vec![1].into_iter().collect();
        imds.retain_interfaces(&fqdn, &live);

        let device = imds.get_device(&fqdn).unwrap();
        assert_eq!(device.interfaces.len(), 1);
        assert!(device.interfaces.contains_key(&1));
    }

    // interface_type is a Prometheus label, so it must stay stable after
    // creation: a refresh reporting a different ifType updates neighbors/
    // speed_override but leaves the type label AND all counters intact.
    // Mutating the label would fabricate a rate() reset by breaking series
    // continuity; a genuine port change gets a new ifindex (fresh entry) and a
    // chassis swap is handled by the base_mac reset.
    #[test]
    fn refresh_interface_update_keeps_type_label_and_counters() {
        let mut imds = test_imds();
        let fqdn = "sw1.example.com".to_string();
        imds.refresh_device(&fqdn, &None);
        imds.refresh_interface(&fqdn, 1, &"ethernetCsmacd".to_string(), &"Gi0/0".to_string(), false, None);
        {
            let iface = imds.interface_mut(&fqdn, 1);
            iface.in_octets = Some(1_000_000);
            iface.up = Some(true);
            iface.speed = Some(1000);
            iface.counter_violations = 3;
        }

        // A later refresh reports a different ifType plus new neighbor/override.
        imds.refresh_interface(&fqdn, 1, &"gigabitEthernet".to_string(), &"Gi0/0".to_string(), true, Some(40000));
        let iface = &imds.get_device(&fqdn).unwrap().interfaces[&1];
        assert_eq!(iface.interface_type, "ethernetCsmacd", "type label is immutable after creation");
        assert_eq!(iface.in_octets, Some(1_000_000), "counters must be preserved");
        assert_eq!(iface.up, Some(true));
        assert_eq!(iface.speed, Some(1000));
        assert_eq!(iface.counter_violations, 3);
        assert_eq!(iface.neighbors, true, "neighbors still updates");
        assert_eq!(iface.speed_override, Some(40000), "speed_override still updates");
    }

    // A chassis replacement behind the same fqdn/ip shows up as a changed
    // base_mac. refresh_device must reset every interface's counters + state so
    // the new hardware's fresh (lower) counters aren't rejected as regressions,
    // and clear the health history that belongs to the removed device.
    #[test]
    fn refresh_device_base_mac_change_resets_interface_counters() {
        let mut imds = test_imds();
        let fqdn = "sw1.example.com".to_string();
        let old_mac = Some("aa:bb:cc:00:00:01".to_string());
        let new_mac = Some("aa:bb:cc:00:00:02".to_string());

        imds.refresh_device(&fqdn, &old_mac);
        imds.refresh_interface(&fqdn, 1, &"ethernetCsmacd".to_string(), &"Gi0/0".to_string(), false, None);
        {
            let iface = imds.interface_mut(&fqdn, 1);
            iface.in_octets = Some(9_000_000);
            iface.up = Some(true);
            iface.speed = Some(1000);
            iface.counter_violations = 7;
            iface.last_report = 1_700_000_000_000; // stale poll time from old hw
        }

        // Same MAC on the next refresh: nothing is disturbed.
        imds.refresh_device(&fqdn, &old_mac);
        assert_eq!(imds.get_device(&fqdn).unwrap().interfaces[&1].in_octets, Some(9_000_000));

        // MAC changes -> chassis swapped -> all interface counters/state reset.
        imds.refresh_device(&fqdn, &new_mac);
        let device = imds.get_device(&fqdn).unwrap();
        assert_eq!(device.base_mac, Some("aa:bb:cc:00:00:02".to_string()));
        let iface = &device.interfaces[&1];
        assert_eq!(iface.in_octets, None);
        assert_eq!(iface.up, None);
        assert_eq!(iface.speed, None);
        assert_eq!(iface.counter_violations, 0);
        // last_report cleared so the first post-swap poll is treated as the
        // baseline, not a huge-interval zero-throughput sample.
        assert_eq!(iface.last_report, 0);
        // Identity is retained — only counters/state are cleared.
        assert_eq!(iface.name, "Gi0/0");
    }

    // A base_mac that differs only in representation (separators/case) is the
    // SAME chassis: discovery emits colon- and space-separated forms and the
    // field is operator-editable, so it must NOT be treated as a swap.
    #[test]
    fn refresh_device_base_mac_representation_change_is_not_a_swap() {
        let mut imds = test_imds();
        let fqdn = "sw1.example.com".to_string();
        imds.refresh_device(&fqdn, &Some("aa:bb:cc:00:00:01".to_string()));
        imds.refresh_interface(&fqdn, 1, &"ethernetCsmacd".to_string(), &"Gi0/0".to_string(), false, None);
        imds.interface_mut(&fqdn, 1).in_octets = Some(500);

        // Same MAC, space-separated and upper-cased.
        imds.refresh_device(&fqdn, &Some("AA BB CC 00 00 01".to_string()));
        assert_eq!(imds.get_device(&fqdn).unwrap().interfaces[&1].in_octets, Some(500),
            "a reformatted MAC must not trigger a swap reset");
    }

    // Learning a base_mac for the first time (None -> Some) or a transient
    // missing MAC (Some -> None) must NOT count as a swap.
    #[test]
    fn refresh_device_base_mac_first_learn_or_missing_is_not_a_swap() {
        let mut imds = test_imds();
        let fqdn = "sw1.example.com".to_string();
        let mac = Some("aa:bb:cc:00:00:01".to_string());

        // Created with no MAC yet, counters accumulate.
        imds.refresh_device(&fqdn, &None);
        imds.refresh_interface(&fqdn, 1, &"ethernetCsmacd".to_string(), &"Gi0/0".to_string(), false, None);
        imds.interface_mut(&fqdn, 1).in_octets = Some(500);

        // None -> Some (first discovery of the MAC): not a swap.
        imds.refresh_device(&fqdn, &mac);
        assert_eq!(imds.get_device(&fqdn).unwrap().interfaces[&1].in_octets, Some(500));
        assert_eq!(imds.get_device(&fqdn).unwrap().base_mac, mac);

        // Some -> None (discovery momentarily didn't report a MAC): not a swap,
        // and the last-known MAC is not clobbered into a false future swap.
        imds.interface_mut(&fqdn, 1).in_octets = Some(600);
        imds.refresh_device(&fqdn, &None);
        assert_eq!(imds.get_device(&fqdn).unwrap().interfaces[&1].in_octets, Some(600));
    }

    #[test]
    fn retain_interfaces_unknown_device_is_noop() {
        let mut imds = test_imds();
        let live: HashSet<i32> = vec![1].into_iter().collect();
        // Must not panic or create the device.
        imds.retain_interfaces(&"ghost.example.com".to_string(), &live);
        assert!(imds.get_device("ghost.example.com").is_none());
    }

    // --- metric rendering ---

    #[test]
    fn get_fast_metrics_encodes_device_and_interface_up() {
        let mut imds = test_imds();
        let up = "up.example.com".to_string();
        let down = "down.example.com".to_string();
        let unknown = "unknown.example.com".to_string();
        for fqdn in [&up, &down, &unknown] {
            imds.refresh_device(fqdn, &None);
        }
        imds.metrics_storage.devices.get_mut(&up).unwrap().up = Some(true);
        imds.metrics_storage.devices.get_mut(&down).unwrap().up = Some(false);

        imds.refresh_interface(&up, 1, &"ethernetCsmacd".to_string(), &"Eth1".to_string(), true, None);
        imds.interface_mut(&up, 1).up = Some(true);

        let metrics = imds.get_fast_metrics();
        let device_up = metrics_by_name(&metrics, "jaspy_device_up");
        // Indeterminate (None) devices emit no up/down metric at all.
        assert_eq!(device_up.len(), 2);
        for metric in device_up {
            let expected = if metric.labels["fqdn"] == up { 1 } else { 0 };
            assert!(matches!(metric.value, MetricValue::Int64(v) if v == expected));
        }

        let iface_up = metrics_by_name(&metrics, "jaspy_interface_up");
        assert_eq!(iface_up.len(), 1);
        assert_eq!(iface_up[0].labels["fqdn"], up);
        assert_eq!(iface_up[0].labels["name"], "Eth1");
        assert_eq!(iface_up[0].labels["neighbors"], "yes");
        assert!(matches!(iface_up[0].value, MetricValue::Int64(1)));
    }

    #[test]
    fn get_metrics_speed_override_wins() {
        let mut imds = test_imds();
        let fqdn = "sw1.example.com".to_string();
        imds.refresh_device(&fqdn, &None);
        imds.refresh_interface(&fqdn, 1, &"ethernetCsmacd".to_string(), &"Eth1".to_string(), false, Some(40000));
        imds.interface_mut(&fqdn, 1).speed = Some(1000);

        let metrics = imds.get_metrics();
        let speed = metrics_by_name(&metrics, "jaspy_interface_speed");
        assert_eq!(speed.len(), 1);
        assert!(matches!(speed[0].value, MetricValue::Int64(40000)));
    }

    #[test]
    fn get_metrics_uses_direction_labels() {
        let mut imds = test_imds();
        let fqdn = "sw1.example.com".to_string();
        imds.refresh_device(&fqdn, &None);
        imds.refresh_interface(&fqdn, 1, &"ethernetCsmacd".to_string(), &"Eth1".to_string(), false, None);
        {
            let iface = imds.interface_mut(&fqdn, 1);
            iface.in_octets = Some(100);
            iface.out_octets = Some(200);
        }

        let metrics = imds.get_metrics();
        let octets = metrics_by_name(&metrics, "jaspy_interface_octets");
        assert_eq!(octets.len(), 2);
        for metric in octets {
            match metric.labels["direction"].as_str() {
                "rx" => assert!(matches!(metric.value, MetricValue::Uint64(100))),
                "tx" => assert!(matches!(metric.value, MetricValue::Uint64(200))),
                other => panic!("unexpected direction label {}", other),
            }
        }
    }

    #[test]
    fn get_metrics_omits_unset_counters() {
        let mut imds = test_imds();
        let fqdn = "sw1.example.com".to_string();
        imds.refresh_device(&fqdn, &None);
        imds.refresh_interface(&fqdn, 1, &"ethernetCsmacd".to_string(), &"Eth1".to_string(), false, None);
        // Everything None: no metrics at all for this interface.
        assert!(imds.get_metrics().is_empty());
    }

    #[test]
    fn get_metrics_neighbors_label_yes_no() {
        let mut imds = test_imds();
        let fqdn = "sw1.example.com".to_string();
        imds.refresh_device(&fqdn, &None);
        imds.refresh_interface(&fqdn, 1, &"ethernetCsmacd".to_string(), &"Eth1".to_string(), true, None);
        imds.refresh_interface(&fqdn, 2, &"ethernetCsmacd".to_string(), &"Eth2".to_string(), false, None);
        imds.interface_mut(&fqdn, 1).in_errors = Some(1);
        imds.interface_mut(&fqdn, 2).in_errors = Some(2);

        let metrics = imds.get_metrics();
        let errors = metrics_by_name(&metrics, "jaspy_interface_errors");
        assert_eq!(errors.len(), 2);
        for metric in errors {
            let expected = if metric.labels["name"] == "Eth1" { "yes" } else { "no" };
            assert_eq!(metric.labels["neighbors"], expected);
        }
    }

    // --- health wiring through the report_interfaces choke point ---

    #[test]
    fn report_interfaces_feeds_flap_and_discard_history() {
        use crate::models::json::InterfaceMonitorReport;
        use diesel::Connection;
        // A migrated in-memory sqlite: the up-change path does a peer lookup;
        // with an empty (but existing) devices table it resolves to no peer.
        let mut conn = AnyConnection::Sqlite(
            diesel::sqlite::SqliteConnection::establish(":memory:").unwrap(),
        );
        crate::db::run_migrations(&mut conn).unwrap();

        let mut imds = test_imds();
        let fqdn = "sw1.example.com".to_string();
        imds.refresh_device(&fqdn, &None);
        imds.refresh_interface(&fqdn, 1, &"ethernetCsmacd".to_string(), &"Eth1".to_string(), false, None);

        let report = |up: Option<bool>, discards: Option<u64>| {
            let mut r = empty_report(1);
            r.up = up;
            r.out_discards = discards;
            InterfaceMonitorReport { device_fqdn: fqdn.clone(), interfaces: vec![r] }
        };

        // First report seeds baseline (up + counter). Sleeps keep last_report
        // strictly increasing (report_interfaces reads the real clock and skips
        // same-millisecond reports).
        imds.report_interfaces(&mut conn, report(Some(true), Some(1000)));
        std::thread::sleep(std::time::Duration::from_millis(2));
        // Flip oper status (1 flap) and grow discards by 5000.
        imds.report_interfaces(&mut conn, report(Some(false), Some(6000)));

        let now = utilities::tools::get_time_msecs();
        let summary = imds.interface_health(&fqdn, 1, now).expect("interface should be unhealthy");
        assert_eq!(summary.flap_count, 1);
        assert_eq!(summary.discards, 5000);
        assert_eq!(summary.severity, Some(crate::utilities::health::Severity::Bad)); // flapping
        // Device rollup reflects the same worst severity.
        assert_eq!(imds.device_health(&fqdn, now), Some(crate::utilities::health::Severity::Bad));
    }

    // Regression: a pinger up-report for a device not yet populated in IMDS (the
    // DB refresh / poller has not run yet at startup) must not be silently
    // dropped. report_device used to early-return for unknown devices, so a
    // reachable device that replied before the first IMDS refresh stayed
    // "unknown" (up=None) forever — the pinger only re-reports on state
    // transitions, so the lost initial "up" was never resent.
    #[test]
    fn report_device_records_up_for_not_yet_known_device() {
        use crate::models::json::DeviceMonitorReport;
        use diesel::Connection;
        // The device-create path does not touch the DB (the peer lookup only
        // runs on an up CHANGE, i.e. when up was already Some), so an unmigrated
        // in-memory connection is sufficient.
        let mut conn = AnyConnection::Sqlite(
            diesel::sqlite::SqliteConnection::establish(":memory:").unwrap(),
        );
        let mut imds = test_imds();
        let fqdn = "tele-sw1.loopback.fi".to_string();

        // Precondition: IMDS has never seen this device (no refresh_device/poll).
        assert!(imds.get_device(&fqdn).is_none());

        // The pinger's first successful ping reports the device up.
        imds.report_device(&mut conn, DeviceMonitorReport { fqdn: fqdn.clone(), up: true });

        // The report must be retained, not dropped.
        let device = imds.get_device(&fqdn)
            .expect("report_device must create an entry for a not-yet-known monitored device");
        assert_eq!(device.up, Some(true), "reported up state must be recorded");
    }
}
