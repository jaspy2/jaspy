use std::time::{SystemTime, UNIX_EPOCH};

pub fn get_time() -> f64 {
    let start = SystemTime::now();
    let unix_timestamp : f64;
    unix_timestamp = start.duration_since(UNIX_EPOCH).unwrap().as_millis() as f64 / 1000.0;
    return unix_timestamp;
}

pub fn get_time_msecs() -> u64 {
    let start = SystemTime::now();
    let unix_timestamp : u64;
    unix_timestamp = start.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
    return unix_timestamp;
}
