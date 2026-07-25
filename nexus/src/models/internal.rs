use crate::utilities::tools;

// Effective feature configuration resolved at startup (main.rs), managed as
// immutable rocket state for GET /api/v1/system.
pub struct SystemInfo {
    pub snmpbot_url: String,
    pub snmp_mode: String, // "snmpbot" | "embedded"
    pub snmp_mib_dir: Option<String>, // embedded mode only
    pub snmp_mibs_loaded: Option<usize>, // embedded mode: resolved table count
    pub trap_receiver_enabled: bool,
    pub trap_bind_address: Option<String>,
    pub poller_enabled: bool,
    pub poll_loop_msecs: u64,
    pub pinger_enabled: bool,
    pub entitypoller_enabled: bool,
    pub entitypoller_interval_msecs: u64,
    pub entitypoller_sensors_enabled: bool,
    pub entitypoller_stp_enabled: bool,
    pub vlanpoller_enabled: bool,
    pub vlanpoller_interval_msecs: u64,
    pub lagpoller_enabled: bool,
    pub lagpoller_interval_msecs: u64,
    pub weathermap_dir: Option<String>,
    pub megaexcel_url: Option<String>, // integration base URL; None disables
    pub db_url: String, // password redacted
    pub db_backend: String, // "postgresql" | "sqlite"
}

pub struct RuntimeInfo {
    pub startup_time: f64,
}

impl RuntimeInfo {
    pub fn new() -> RuntimeInfo {
        return RuntimeInfo {
            startup_time: tools::get_time(),
        };
    }
    pub fn state_id(self: &RuntimeInfo) -> i64 {
        let start_time = self.startup_time;
        let state_id = (start_time * 100000.0) as i64;
        return state_id;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_id_scales_startup_time() {
        let info = RuntimeInfo { startup_time: 1234.56789 };
        assert_eq!(info.state_id(), 123456789);
    }

    #[test]
    fn state_id_truncates_sub_resolution_digits() {
        let info = RuntimeInfo { startup_time: 1.000009 };
        assert_eq!(info.state_id(), 100000);
    }
}
