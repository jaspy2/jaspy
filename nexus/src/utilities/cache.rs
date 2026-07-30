use std::sync::{Arc, Mutex};
use crate::models::json;
use crate::utilities::tools;
use crate::utilities::issues::TrackedIssue;
use std::ops::DerefMut;

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

// A short-lived snapshot of the derived+reconciled fleet issue list. Reading the
// issue list is heavy (N+1 DB queries, many store locks, per-VLAN STP trees), so
// concurrent /issues and per-device views share this snapshot instead of each
// re-deriving. The background issue_scan_worker refreshes it on its own cadence;
// reads rebuild it only on a miss. `ttl_secs` matches the scan interval.
pub struct CachedIssues {
    pub valid_until: f64,
    pub issues: Vec<TrackedIssue>,
}

impl CachedIssues {
    pub fn new(issues: Vec<TrackedIssue>, ttl_secs: f64) -> CachedIssues {
        CachedIssues {
            valid_until: tools::get_time() + ttl_secs,
            issues,
        }
    }
}

pub struct CacheController {
    pub cached_weathermap_topology: Arc<Mutex<Option<CachedWeathermapTopology>>>,
    pub cached_issues: Arc<Mutex<Option<CachedIssues>>>,
}

impl CacheController {
    pub fn new() -> CacheController {
        return CacheController {
            cached_weathermap_topology: Arc::new(Mutex::new(None)),
            cached_issues: Arc::new(Mutex::new(None)),
        }
    }

    pub fn invalidate_weathermap_cache(self: &CacheController) {
        if let Ok(ref mut cached_weathermap_topology_option_mutex) = self.cached_weathermap_topology.lock() {
            let cached_weathermap_topology_option: &mut Option<CachedWeathermapTopology> = cached_weathermap_topology_option_mutex.deref_mut();
            if let Some(cached_weathermap_topology_data) = cached_weathermap_topology_option {
                cached_weathermap_topology_data.valid_until = 0.0;
            }
        }
    }

    // Force the next issue-list read to rebuild (used after a fleet change or a
    // suppression toggle so the effect is immediate, not up to one TTL late).
    pub fn invalidate_issues_cache(self: &CacheController) {
        if let Ok(ref mut option) = self.cached_issues.lock() {
            if let Some(cached) = option.deref_mut() {
                cached.valid_until = 0.0;
            }
        }
    }

    // Store a freshly derived+reconciled snapshot.
    pub fn store_issues(self: &CacheController, issues: Vec<TrackedIssue>, ttl_secs: f64) {
        if let Ok(mut option) = self.cached_issues.lock() {
            *option = Some(CachedIssues::new(issues, ttl_secs));
        }
    }

    // Return the cached snapshot if still fresh; None on miss/expiry.
    pub fn fresh_issues(self: &CacheController) -> Option<Vec<TrackedIssue>> {
        if let Ok(option) = self.cached_issues.lock() {
            if let Some(cached) = option.as_ref() {
                if tools::get_time() < cached.valid_until {
                    return Some(cached.issues.clone());
                }
            }
        }
        None
    }
}
