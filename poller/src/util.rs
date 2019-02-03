use std::time::{SystemTime, UNIX_EPOCH};

pub fn get_time_msecs() -> u64 {
    let start = SystemTime::now();
    match start.duration_since(UNIX_EPOCH) {
        Ok(unix_time) => {
            let in_ms = unix_time.as_secs() * 1000 + unix_time.subsec_nanos() as u64 / 1_000_000;
            return in_ms;
        },
        Err(_) => { return 0; }
    }
}