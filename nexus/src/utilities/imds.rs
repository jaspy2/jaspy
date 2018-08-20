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

    pub fn refresh_interface(self: &mut IMDS, device_fqdn: &String, if_index: i32, name: &String, neighbors: bool, speed_override: Option<i32>) {
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
        match self.metrics_storage.devices.get_mut(&imr.device_fqdn) {
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
            match device.interfaces.get_mut(&interface_report.if_index) {
                Some(target_interface) => { interface = target_interface; },
                None => {
                    // TODO: log? this means we got a report for interface we don't really follow
                    continue;
                }
            }

            // TODO: statechanges should be emitted
            if interface_report.in_octets.is_some() { interface.in_octets = interface_report.in_octets; }
            if interface_report.out_octets.is_some() { interface.out_octets = interface_report.out_octets; }
            if interface_report.in_packets.is_some() { interface.in_packets = interface_report.in_packets; }
            if interface_report.out_packets.is_some() { interface.out_packets = interface_report.out_packets; }
            if interface_report.in_errors.is_some() { interface.in_errors = interface_report.in_errors; }
            if interface_report.out_errors.is_some() { interface.out_errors = interface_report.out_errors; }
            if interface_report.up.is_some() { interface.up = interface_report.up; }
            if interface_report.speed.is_some() { interface.speed = interface_report.speed; }
        }
    }

    pub fn get_fast_metrics(self: &IMDS) {
        let jaspy_device_up = "jaspy_device_up".to_string();
        let mut metric_values: Vec<models::metrics::MetricValue> = Vec::new();
        for (_device_key, device_metrics) in self.metrics_storage.devices.iter() {
            // TODO: still needs labels
            // Only emit device up/down metrics if device is actually up/down aka. not indeterminate :)
            if let Some(device_up_bool) = device_metrics.up {
                let device_up : i64;
                if device_up_bool { device_up = 1; } else { device_up = 0; }
                let metric = models::metrics::LabeledMetric::new(&jaspy_device_up, models::metrics::MetricValue::Int64(device_up), &HashMap::new());
            }
            
            println!("{} status={:?}", device_metrics.fqdn, device_metrics.up);
            for (_interface_key, interface_metrics) in device_metrics.interfaces.iter() {
                let reported_speed = match interface_metrics.speed_override {
                    Some(speed_override) => Some(speed_override),
                    None => interface_metrics.speed
                };
                println!(" {} status={:?} speed={:?}", interface_metrics.name, interface_metrics.up, reported_speed);
            }
        }
    }
}