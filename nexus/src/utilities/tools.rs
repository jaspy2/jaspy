extern crate time;

pub fn get_time() -> f64 {
    let current_time = time::get_time();
    let unix_timestamp = (current_time.sec as f64) + ((current_time.nsec as f64) * 1e-09);
    return unix_timestamp;
}

pub fn get_time_msecs() -> u64 {
    let current_time = time::get_time();
    let unix_timestamp = (current_time.sec as f64) + ((current_time.nsec as f64) * 1e-09);
    let msecs = unix_timestamp * 1000.0;
    return msecs as u64;
}
