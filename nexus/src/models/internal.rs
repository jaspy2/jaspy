use crate::utilities::tools;

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
