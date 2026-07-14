use diesel::pg::PgConnection;
use diesel::r2d2::{ConnectionManager, PooledConnection};
use std::env;
use std::ops::{Deref, DerefMut};
use rocket::http::Status;
use rocket::request::{FromRequest, Outcome};
use rocket::Request;

pub type Pool = diesel::r2d2::Pool<ConnectionManager<PgConnection>>;
pub type PooledConn = PooledConnection<ConnectionManager<PgConnection>>;

pub fn connect() -> Pool {
    let env_opt = env::var("JASPY_DB_URL");
    match env_opt {
        Ok(env_opt) => {
            let manager = ConnectionManager::<PgConnection>::new(env_opt);
            diesel::r2d2::Pool::builder().build(manager).expect("Failed to create pool")
        },
        Err(_) => {
            panic!("JASPY_DB_URL env var not set!");
        }
    }
}

// Request guard that checks out a pooled connection for the duration of a
// request. Replaces the rocket_contrib `#[database]` fairing from Rocket 0.4.
// Derefs to PgConnection so handlers can call diesel-2 model methods with
// `&mut *connection`.
pub struct JaspyDB(pub PooledConn);

impl Deref for JaspyDB {
    type Target = PgConnection;
    fn deref(&self) -> &PgConnection { &*self.0 }
}

impl DerefMut for JaspyDB {
    fn deref_mut(&mut self) -> &mut PgConnection { &mut *self.0 }
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for JaspyDB {
    type Error = ();
    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        match request.rocket().state::<Pool>() {
            Some(pool) => match pool.get() {
                Ok(conn) => Outcome::Success(JaspyDB(conn)),
                Err(_) => Outcome::Error((Status::ServiceUnavailable, ())),
            },
            None => Outcome::Error((Status::InternalServerError, ())),
        }
    }
}
