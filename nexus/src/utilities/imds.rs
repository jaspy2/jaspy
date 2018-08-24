use models;
use std::collections::HashMap;
use std::sync::{Arc,Mutex};
use utilities;

pub struct IMDS {
    metrics_storage : models::metrics::Metrics,
    msgbus: Arc<Mutex<utilities::msgbus::MessageBus>>
}

impl IMDS {
    pub fn new(msgbus: Arc<Mutex<utilities::msgbus::MessageBus>>) -> IMDS {
        let imds = IMDS {
            metrics_storage: models::metrics::Metrics {
                devices: HashMap::new()
            },
            msgbus: msgbus
        };

        return imds;
    }

    pub fn prune(self: &mut IMDS) {
        let current_time = utilities::tools::get_time();
        let mut delete_device_keys: Vec<String> = Vec::new();
        for (fqdn, device_metrics) in self.metrics_storage.devices.iter_mut() {
            let mut delete_ifindex_keys: Vec<i32> = Vec::new();
            if current_time > device_metrics.expiry {
                delete_device_keys.push(fqdn.clone());
            } else {
                for (ifindex, interface_metrics) in device_metrics.interfaces.iter() {
                    if current_time > interface_metrics.expiry {
                        delete_ifindex_keys.push(*ifindex);
                    }
                }
                for iface_key in delete_ifindex_keys.iter() {
                    device_metrics.interfaces.remove(iface_key);
                }
            }
        }
        for device_key in delete_device_keys.iter() {
            self.metrics_storage.devices.remove(device_key);
        }
    }

    pub fn get_device(self: &IMDS, device_fqdn: &String) -> Option<&models::metrics::DeviceMetrics> {
        return self.metrics_storage.devices.get(device_fqdn);
    }

    pub fn refresh_device(self: &mut IMDS, device_fqdn: &String) {
        match self.metrics_storage.devices.get_mut(device_fqdn) {
            Some(device) => {
                device.expiry = utilities::tools::get_time() + 60.0;
                return;
            },
            None => {}
        }
        let dm = models::metrics::DeviceMetrics {
            expiry: utilities::tools::get_time() + 60.0,
            fqdn: device_fqdn.clone(),
            up: None,
            interfaces: HashMap::new(),
        };
        self.metrics_storage.devices.insert(device_fqdn.clone(), dm);
    }

    pub fn report_device(self: &mut IMDS, dmr: models::json::DeviceMonitorReport) {
        let device;
        match self.metrics_storage.devices.get_mut(&dmr.fqdn) {
            Some(value) => {
                device = value;
            },
            None => {
                // TODO: log? this means we got a report from a host that is not being monitored, it is possible this is normal on device removal
                return;
            }
        }
        
        if let Some(device_up) = device.up {
            if device_up != dmr.up {
                if let Ok(ref mut msgbus) = self.msgbus.lock() {
                    let msg;
                    if dmr.up { 
                        msg = format!("{} UP", dmr.fqdn);
                    } else {
                        msg = format!("{} DOWN", dmr.fqdn);
                    }
                    msgbus.message_str(&msg);
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
                // TODO: events!
                if target_interface.name != *name { target_interface.name = name.clone(); }
                target_interface.neighbors = neighbors;
                target_interface.speed_override = speed_override;
                target_interface.expiry = utilities::tools::get_time() + 60.0;
                return;
            },
            None => {}
        }
        device.interfaces.insert(if_index, models::metrics::InterfaceMetrics {
            expiry: utilities::tools::get_time() + 60.0,
            name: name.clone(),
            neighbors: neighbors,
            speed_override: speed_override,
            interface_type: interface_type.clone(),

            in_octets: None,
            out_octets: None,
            in_packets: None,
            out_packets: None,
            in_errors: None,
            out_errors: None,
            up: None,
            speed: None,
        });
    }

    pub fn report_interfaces(self: &mut IMDS, imr: models::json::InterfaceMonitorReport) {
        let device;
        match self.metrics_storage.devices.get_mut(&imr.deviceFqdn) {
            Some(value) => {
                device = value;
            },
            None => {
                // TODO: log? this means we got a report from a host that is not being monitored, it is possible this is normal on device removal
                return;
            }
        }
        for interface_report in imr.interfaces.iter() {
            let mut interface;
            match device.interfaces.get_mut(&interface_report.ifIndex) {
                Some(target_interface) => { interface = target_interface; },
                None => {
                    // TODO: log? this means we got a report for interface we don't really follow
                    continue;
                }
            }

            // TODO: statechanges should be emitted
            if interface_report.inOctets.is_some() { interface.in_octets = interface_report.inOctets; }
            if interface_report.outOctets.is_some() { interface.out_octets = interface_report.outOctets; }
            if interface_report.inPackets.is_some() { interface.in_packets = interface_report.inPackets; }
            if interface_report.outPackets.is_some() { interface.out_packets = interface_report.outPackets; }
            if interface_report.inErrors.is_some() { interface.in_errors = interface_report.inErrors; }
            if interface_report.outErrors.is_some() { interface.out_errors = interface_report.outErrors; }
            if interface_report.up.is_some() { interface.up = interface_report.up; }
            if interface_report.speed.is_some() { interface.speed = interface_report.speed; }
        }
    }

    pub fn get_metrics(self: &IMDS) -> Vec<models::metrics::LabeledMetric> {
        let jaspy_interface_octets = "jaspy_interface_octets".to_string();
        let jaspy_interface_packets = "jaspy_interface_packets".to_string();
        let jaspy_interface_errors = "jaspy_interface_errors".to_string();
        let jaspy_interface_speed = "jaspy_interface_speed".to_string();

        let mut metric_values: Vec<models::metrics::LabeledMetric> = Vec::new();
        for (_device_key, device_metrics) in self.metrics_storage.devices.iter() {
            for (_interface_key, interface_metrics) in device_metrics.interfaces.iter() {
                let reported_speed = match interface_metrics.speed_override {
                    Some(speed_override) => Some(speed_override),
                    None => interface_metrics.speed
                };

                let mut labels: HashMap<String,String> = HashMap::new();
                labels.insert("fqdn".to_string(), device_metrics.fqdn.clone());
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
                        &labels
                    ));
                }

                if let Some(interface_metrics_in_octets) = interface_metrics.in_octets {
                    metric_values.push(models::metrics::LabeledMetric::new(
                        &jaspy_interface_octets, models::metrics::MetricValue::Uint64(interface_metrics_in_octets),
                        &in_labels
                    ));
                }

                if let Some(interface_metrics_out_octets) = interface_metrics.out_octets {
                    metric_values.push(models::metrics::LabeledMetric::new(
                        &jaspy_interface_octets, models::metrics::MetricValue::Uint64(interface_metrics_out_octets),
                        &out_labels
                    ));
                }

                if let Some(interface_metrics_in_packets) = interface_metrics.in_packets {
                    metric_values.push(models::metrics::LabeledMetric::new(
                        &jaspy_interface_packets, models::metrics::MetricValue::Uint64(interface_metrics_in_packets),
                        &in_labels
                    ));
                }

                if let Some(interface_metrics_out_packets) = interface_metrics.out_packets {
                    metric_values.push(models::metrics::LabeledMetric::new(
                        &jaspy_interface_packets, models::metrics::MetricValue::Uint64(interface_metrics_out_packets),
                        &out_labels
                    ));
                }

                if let Some(interface_metrics_in_errors) = interface_metrics.in_errors {
                    metric_values.push(models::metrics::LabeledMetric::new(
                        &jaspy_interface_errors, models::metrics::MetricValue::Uint64(interface_metrics_in_errors),
                        &in_labels
                    ));
                }

                if let Some(interface_metrics_out_errors) = interface_metrics.out_errors {
                    metric_values.push(models::metrics::LabeledMetric::new(
                        &jaspy_interface_errors, models::metrics::MetricValue::Uint64(interface_metrics_out_errors),
                        &out_labels
                    ));
                }
            }
        }

        return metric_values;
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
                if device_up_bool { device_up = 1; } else { device_up = 0; }
                let metric = models::metrics::LabeledMetric::new(
                    &jaspy_device_up, models::metrics::MetricValue::Int64(device_up),
                    &labels
                );
                metric_values.push(metric);
            }

            for (_interface_key, interface_metrics) in device_metrics.interfaces.iter() {
                let mut labels: HashMap<String,String> = HashMap::new();
                labels.insert("fqdn".to_string(), device_metrics.fqdn.clone());
                labels.insert("name".to_string(), interface_metrics.name.clone());
                labels.insert("interface_type".to_string(), interface_metrics.interface_type.clone());
                if interface_metrics.neighbors { labels.insert("neighbors".to_string(), "yes".to_string()); }
                else { labels.insert("neighbors".to_string(), "no".to_string()); }

                if let Some(interface_metrics_up) = interface_metrics.up {
                    let val : i64;
                    if interface_metrics_up { val = 1; } else { val = 0; }
                    metric_values.push(models::metrics::LabeledMetric::new(
                        &jaspy_interface_up, models::metrics::MetricValue::Int64(val),
                        &labels
                    ));
                }
            }
        }

        return metric_values;
    }
}