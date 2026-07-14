// Discovery HTTP surface. The ingest endpoints (PUT /device, PUT /links) are
// thin wrappers over utilities::discovery, which the in-process discovery
// engine (collectors/discovery.rs) also calls directly. The run/status/config
// endpoints control that engine; they are the hook points for the future web
// admin interface.
use crate::models;
use crate::db;
use crate::utilities;
use crate::collectors::discovery::DiscoveryControl;
use rocket::{get, put, post};
use rocket::http::Status;
use rocket::serde::json::Json;
use rocket::State;
use std::sync::{Arc, Mutex};

// TODO: GH#9 Move everything to v1 API
#[put("/device", data = "<discovery_json>")]
pub fn discovery_device(
    discovery_json: Json<models::json::DiscoveredDevice>,
    mut connection: db::JaspyDB,
    msgbus: &State<Arc<Mutex<utilities::msgbus::MessageBus>>>,
    cache_controller: &State<Arc<Mutex<utilities::cache::CacheController>>>,
) {
    utilities::discovery::ingest_device(&mut connection, msgbus.inner(), cache_controller.inner(), &discovery_json.into_inner());
}

#[put("/links", data = "<links_json>")]
pub fn discovery_links(
    links_json: Json<models::json::LinkInfo>,
    mut connection: db::JaspyDB,
    cache_controller: &State<Arc<Mutex<utilities::cache::CacheController>>>,
) {
    utilities::discovery::ingest_links(&mut connection, cache_controller.inner(), &links_json.into_inner());
}

// --- in-process discovery engine control ---

#[post("/run", data = "<run_json>")]
pub fn discovery_run(
    run_json: Option<Json<models::json::DiscoveryRunRequest>>,
    control: &State<Arc<Mutex<DiscoveryControl>>>,
) -> (Status, Json<models::json::DiscoveryStatus>) {
    let overrides = run_json.map(|j| j.into_inner());
    if let Ok(mut control) = control.inner().lock() {
        if control.status.running || control.trigger_requested {
            return (Status::Conflict, Json(control.status_dto()));
        }
        // Validate that a run is actually possible with config + overrides.
        let root_ok = overrides.as_ref().and_then(|o| o.root_device.clone()).or_else(|| control.config.root_device.clone()).is_some();
        let community_ok = overrides.as_ref().and_then(|o| o.community.clone()).or_else(|| control.config.community.clone()).is_some();
        if !root_ok || !community_ok {
            return (Status::BadRequest, Json(control.status_dto()));
        }
        control.trigger_requested = true;
        control.trigger_overrides = overrides;
        return (Status::Accepted, Json(control.status_dto()));
    }
    (Status::InternalServerError, Json(models::json::DiscoveryStatus::default()))
}

#[get("/status")]
pub fn discovery_status(control: &State<Arc<Mutex<DiscoveryControl>>>) -> Json<models::json::DiscoveryStatus> {
    if let Ok(control) = control.inner().lock() {
        return Json(control.status_dto());
    }
    Json(models::json::DiscoveryStatus::default())
}

#[get("/config")]
pub fn discovery_get_config(control: &State<Arc<Mutex<DiscoveryControl>>>) -> Json<models::json::DiscoveryConfig> {
    if let Ok(control) = control.inner().lock() {
        return Json(control.config.clone());
    }
    Json(models::json::DiscoveryConfig::default())
}

#[put("/config", data = "<config_json>")]
pub fn discovery_put_config(
    config_json: Json<models::json::DiscoveryConfig>,
    mut connection: db::JaspyDB,
    control: &State<Arc<Mutex<DiscoveryControl>>>,
) -> Result<Json<models::json::DiscoveryConfig>, Status> {
    let new_config = config_json.into_inner();
    // Persist so the config survives restarts (wins over the env seed). A
    // failed write must surface: silently reverting to the env seed on the
    // next restart would be worse than an error now.
    let json = serde_json::to_string(&new_config).map_err(|_| Status::InternalServerError)?;
    if let Err(e) = models::dbo::Setting::set(&mut connection, "discovery_config", &json) {
        println!("[discovery] failed to persist config: {}", e);
        return Err(Status::InternalServerError);
    }
    if let Ok(mut control) = control.inner().lock() {
        control.config = new_config;
        return Ok(Json(control.config.clone()));
    }
    Err(Status::InternalServerError)
}
