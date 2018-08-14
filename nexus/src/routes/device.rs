extern crate rocket_contrib;
use models;
use db;
use rocket_contrib::Json;

#[get("/")]
fn device_list(connection: db::Connection) -> Json<Vec<models::dbo::Device>> {
    return Json(models::dbo::Device::all(&connection));
}

#[get("/monitor")]
fn monitored_device_list(connection: db::Connection) -> Json<models::json::DeviceMonitorResponse> {
    // TODO: initial responsive status from DB, do we need indeterminate state?
    let mut dmi : Vec<models::json::DeviceMonitorInfo> = Vec::new();
    for monitored in models::dbo::Device::monitored(&connection).iter() {
        let fqdn = format!("{}.{}", monitored.name, monitored.dns_domain);
        dmi.push(models::json::DeviceMonitorInfo { fqdn: fqdn, responsive: true });
    }
    return Json(models::json::DeviceMonitorResponse { devices : dmi });
}