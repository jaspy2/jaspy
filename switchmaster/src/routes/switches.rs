extern crate rocket_contrib;

use rocket::{get, post, put, delete};
use rocket_contrib::json::Json;
use rocket::response::status::Created;
use super::super::models::switch::{Switch, NewSwitch, UpdatedSwitch};
use super::super::repositories::switch::{SwitchRepository};
use kerosiini::repository::{Repository, RepositoryError};

#[get("/?<page>&<page_size>")]
pub fn list(repository: SwitchRepository, page: Option<i64>, page_size: Option<i64>) -> Result<Json<Vec<Switch>>, RepositoryError> {
    repository.paginate(page.unwrap_or(1), page_size.unwrap_or(25))
        .map(|switches| Json(switches.records))
}

#[get("/<id>")]
pub fn find(repository: SwitchRepository, id: i64) -> Result<Json<Switch>, RepositoryError> {
    repository.find(id)
        .map(|switch| Json(switch))
}

#[post("/", data = "<new_switch>")]
pub fn create(repository: SwitchRepository, new_switch: Json<NewSwitch>) -> Result<Created<Json<Switch>>, RepositoryError> {
    match repository.add(new_switch.into_inner()) {
        Ok(created_switch) => {
            let switch_uri = uri!(find: created_switch.id);
            Ok(Created(switch_uri.to_string(), Some(Json(created_switch))))
        },
        Err(err) => Err(err)
    }
}

#[put("/<id>", data = "<update_switch>")]
pub fn update(repository: SwitchRepository, id: i64, update_switch: Json<UpdatedSwitch>) -> Result<(), RepositoryError> {
    repository.update(id, update_switch.into_inner())
        .map(|_| ())
}

#[delete("/<id>")]
pub fn delete(repository: SwitchRepository, id: i64) -> Result<Json<Switch>, RepositoryError> {
    repository.delete(id)
        .map(|switch| Json(switch))
}
