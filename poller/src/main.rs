#[macro_use] extern crate serde_derive;
#[macro_use] extern crate serde_json;
extern crate reqwest;
extern crate rand as crate_rand;
mod models;
mod poller;
mod util;
use std::time;
use std::collections::HashMap;
use std::thread;


const MAIN_LOOP_MSECS : u64 = 1000;

fn print_usage() {
    println!("usage: {} <jaspy_url> <snmpbot_url>", std::env::args().nth(0).unwrap());
}

fn get_devices(jaspy_url : &String) -> Result<HashMap<String, models::json::Device>, String> {
    let source_url = format!("{}/device", jaspy_url);
    let mut devices : HashMap<String, models::json::Device> = HashMap::new();
    if let Ok(mut response) = reqwest::get(&source_url) {
        let resp_json : Result<Vec<models::json::Device>, _> = response.json();
        if let Ok(device_list) = resp_json {
            for device_info in device_list.iter() {
                if let Some(polling_enabled) = device_info.polling_enabled {
                    if !polling_enabled {
                        continue;
                    }
                }
                let fqdn = format!("{}.{}", device_info.name, device_info.dns_domain);
                devices.insert(fqdn, device_info.clone());
            }
        } else {
            return Err("failed to parse JSON from jaspy".to_string());
        }
    } else {
        return Err("failed to get device information from jaspy".to_string());
    }

    return Ok(devices);
}

fn main() {
    let jaspy_url;
    let snmpbot_url;
    let mut poll_workers : HashMap<String, poller::PollThreadInfo> = HashMap::new();
    let mut reap_threads : Vec<poller::PollThreadInfo> = Vec::new();
    let mut devices : HashMap<String, models::json::Device> = HashMap::new();


    if let Some(argv1) = std::env::args().nth(1) {
        jaspy_url = argv1;
    } else {
        print_usage();
        return;
    }

    if let Some(argv2) = std::env::args().nth(2) {
        snmpbot_url = argv2;
    } else {
        print_usage();
        return;
    }
    
    loop {
        match get_devices(&jaspy_url) {
            Ok(received_devices) => {
                devices = received_devices;
            },
            Err(error) => {
                println!("Failed to get new device listing: {}", error);
            }
        }
        let mut expired_fqdns : Vec<String> = Vec::new();
        poller::check_if_worker_needed(&jaspy_url, &snmpbot_url, &devices, &mut poll_workers);
        poller::check_expired_fqdn_workers(&devices, &poll_workers, &mut expired_fqdns);
        poller::prepare_expired_fqdns_for_reap(&mut poll_workers, &expired_fqdns, &mut reap_threads);
        poller::reap_finished_threads(&mut reap_threads);
        thread::sleep(time::Duration::from_millis(MAIN_LOOP_MSECS));
    }
}