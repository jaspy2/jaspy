use diesel::pg::PgConnection;
use diesel::r2d2::{ConnectionManager, PooledConnection};
use std::env;
use std::ops::{Deref, DerefMut};
use rocket::http::Status;
use rocket::request::{FromRequest, Outcome};
use rocket::Request;

pub type Pool = diesel::r2d2::Pool<ConnectionManager<PgConnection>>;
pub type PooledConn = PooledConnection<ConnectionManager<PgConnection>>;

// JASPY_DB_URL with any password replaced, safe for logs.
fn redacted_db_url(db_url: &str) -> String {
    match reqwest::Url::parse(db_url) {
        Ok(mut url) => {
            if url.password().is_some() {
                let _ = url.set_password(Some("***"));
            }
            url.to_string()
        },
        Err(_) => "<unparseable JASPY_DB_URL>".to_string(),
    }
}

pub fn connect() -> Pool {
    let db_url = match env::var("JASPY_DB_URL") {
        Ok(db_url) => db_url,
        Err(_) => {
            panic!("JASPY_DB_URL env var not set!");
        }
    };
    // The pool builder blocks retrying the first connection for up to 30s
    // before giving up, which looks like a silent hang — announce it first.
    println!("[db] connecting to {} (waits up to 30s before giving up)", redacted_db_url(&db_url));
    let manager = ConnectionManager::<PgConnection>::new(db_url.clone());
    match diesel::r2d2::Pool::builder().build(manager) {
        Ok(pool) => pool,
        Err(e) => {
            panic!(
                "failed to connect to {}: {} — check that postgres is running, \
                 the role in the URL exists, and JASPY_DB_URL survives sudo/systemd \
                 (running as a different user changes unix-socket authentication)",
                redacted_db_url(&db_url), e
            );
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
