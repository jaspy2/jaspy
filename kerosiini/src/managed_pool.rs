use std::ops::Deref;
use rocket::http::Status;
use rocket::request::{self, FromRequest};
use rocket::{Request, State, Outcome};
use r2d2;
use r2d2_diesel::ConnectionManager;
use diesel::pg::PgConnection;

pub type Pool = r2d2::Pool<ConnectionManager<PgConnection>>;

pub struct Connection(pub r2d2::PooledConnection<ConnectionManager<PgConnection>>);

impl<'a, 'r> FromRequest<'a, 'r> for Connection {
    type Error = ();

    fn from_request(request: &'a Request<'r>) -> request::Outcome<Connection, ()> {
        let pool = request.guard::<State<Pool>>()?;
        match pool.get() {
            Ok(conn) => Outcome::Success(Connection(conn)),
            Err(_) => Outcome::Failure((Status::ServiceUnavailable, ()))
        }
    }
}

impl Deref for Connection {
    type Target = PgConnection;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub fn create_managed_pool(connection_string: String) -> Pool {
    let manager = ConnectionManager::<PgConnection>::new(connection_string);
    return r2d2::Pool::builder().build(manager).expect("Failed to create pool");
}
