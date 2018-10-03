use std::sync::{Arc, Mutex};
use models::json;
use utilities::tools;

pub struct CachedWeathermapTopology {
    pub valid_until: f64,
    pub weathermap_topology: json::WeathermapBase,
}

impl CachedWeathermapTopology {
    pub fn new(topology: json::WeathermapBase) -> CachedWeathermapTopology {
        return CachedWeathermapTopology {
            // TODO: configurable cache time :)
            valid_until: tools::get_time() + 30.0,
            weathermap_topology: topology
        };
    }
}

pub struct CacheController {
    pub cached_weathermap_topology: Arc<Mutex<Option<CachedWeathermapTopology>>>
}

impl CacheController {
    pub fn new() -> CacheController {
        return CacheController {
            cached_weathermap_topology: Arc::new(Mutex::new(None)),
        }
    }
}