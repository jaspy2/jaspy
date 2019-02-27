use super::super::schema::*;
use diesel::{Insertable, Queryable};

#[derive(Serialize, Deserialize, Queryable)]
#[serde(rename_all = "camelCase")]
pub struct Switch {
    pub id: i64,
    pub display_name: String,
    pub configured: bool,
    pub deploy_state: String,
}

#[table_name = "switches"]
#[derive(Deserialize, Insertable)]
#[serde(rename_all = "camelCase")]
pub struct NewSwitch {
    pub display_name: String,
    pub configured: bool,
    pub deploy_state: String
}

#[table_name = "switches"]
#[derive(Deserialize, AsChangeset)]
#[serde(rename_all = "camelCase")]
pub struct UpdatedSwitch {
    pub display_name: String,
    pub configured: bool,
    pub deploy_state: String
}
