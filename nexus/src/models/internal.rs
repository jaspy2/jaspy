use crate::utilities::tools;

// Effective feature configuration resolved at startup (main.rs), managed as
// immutable rocket state for GET /api/v1/system.
pub struct SystemInfo {
    pub snmpbot_url: String,
    pub poller_enabled: bool,
    pub poll_loop_msecs: u64,
    pub pinger_enabled: bool,
    pub entitypoller_enabled: bool,
    pub entitypoller_interval_msecs: u64,
    pub entitypoller_sensors_enabled: bool,
    pub entitypoller_stp_enabled: bool,
    pub weathermap_dir: Option<String>,
    pub db_url: String, // password redacted
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
