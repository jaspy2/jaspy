use diesel::pg::PgConnection;
use std::env;
use r2d2;
use r2d2_diesel::ConnectionManager;
use rocket_contrib::databases::diesel;

pub type Pool = r2d2::Pool<ConnectionManager<PgConnection>>;

#[database("jaspy_db")]
pub struct JaspyDB(diesel::PgConnection);

pub fn connect() -> Pool {
    let env_opt = env::var("JASPY_DB_URL");
    match env_opt {
        Ok(env_opt) => {
            let manager = ConnectionManager::<PgConnection>::new(env_opt);
            r2d2::Pool::builder().build(manager).expect("Failed to create pool")
        },
        Err(_) => {
            panic!("JASPY_DB_URL env var not set!");
        }
    }
}

