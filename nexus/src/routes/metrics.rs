extern crate rocket_contrib;
use std::sync::{Arc, Mutex};
use utilities;
use rocket::State;

#[get("/fast")]
fn metrics_fast(imds: State<Arc<Mutex<utilities::imds::IMDS>>>) {
    if let Ok(ref mut imds) = imds.inner().lock() {
        imds.get_fast_metrics();
    }
}