use diesel::connection::SimpleConnection;
use diesel::pg::PgConnection;
use diesel::sqlite::SqliteConnection;
use diesel::r2d2::R2D2Connection;
use diesel::Connection;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use std::env;
use std::ops::{Deref, DerefMut};
use std::sync::OnceLock;
use rocket::http::Status;
use rocket::request::{FromRequest, Outcome};
use rocket::Request;

// Runtime-selectable database backend: JASPY_DB_URL decides.
//   postgres://... | postgresql://...  -> PostgreSQL
//   sqlite://path | sqlite:path | path -> SQLite
//
// The MultiConnection derive gives us Connection/LoadConnection/
// R2D2Connection over the enum for all backend-agnostic queries. The two
// idioms it cannot express generically (ON CONFLICT upserts and
// INSERT..RETURNING/get_result) are dispatched per-variant with the
// with_backend! macro below.
//
// The Sqlite variant must stay LAST: the derived establish() tries variants
// in declaration order, and sqlite accepts almost any string as a path.
#[derive(diesel::MultiConnection)]
pub enum AnyConnection {
    Postgresql(PgConnection),
    Sqlite(SqliteConnection),
}

// Runs the same backend-agnostic diesel DSL body against the concrete
// connection of whichever variant is active. Each match arm monomorphizes
// $body for that backend, so backend-capable-but-enum-unsupported constructs
// (on_conflict, get_result on insert) compile fine.
#[macro_export]
macro_rules! with_backend {
    ($any:expr, |$conn:ident| $body:expr) => {
        match $any {
            $crate::db::AnyConnection::Postgresql($conn) => $body,
            $crate::db::AnyConnection::Sqlite($conn) => $body,
        }
    };
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BackendKind {
    Postgres,
    Sqlite,
}

impl BackendKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            BackendKind::Postgres => "postgresql",
            BackendKind::Sqlite => "sqlite",
        }
    }
}

pub fn backend_kind(db_url: &str) -> BackendKind {
    if db_url.starts_with("postgres://") || db_url.starts_with("postgresql://") {
        BackendKind::Postgres
    } else {
        BackendKind::Sqlite
    }
}

// "sqlite:///var/lib/jaspy.db" / "sqlite:jaspy.db" / bare path -> filesystem path.
pub fn sqlite_path(db_url: &str) -> &str {
    db_url
        .strip_prefix("sqlite://")
        .or_else(|| db_url.strip_prefix("sqlite:"))
        .unwrap_or(db_url)
}

// JASPY_DB_URL safe for logs: postgres passwords replaced; sqlite paths carry
// no secrets and pass through unchanged.
pub fn redacted_db_url(db_url: &str) -> String {
    match backend_kind(db_url) {
        BackendKind::Sqlite => db_url.to_string(),
        BackendKind::Postgres => match reqwest::Url::parse(db_url) {
            Ok(mut url) => {
                if url.password().is_some() {
                    let _ = url.set_password(Some("***"));
                }
                url.to_string()
            },
            Err(_) => "<unparseable JASPY_DB_URL>".to_string(),
        },
    }
}

// Per-connection sqlite setup. WAL persists in the database file, but
// busy_timeout / foreign_keys / synchronous are connection-scoped, so this
// runs for every pooled and direct connection.
fn apply_sqlite_pragmas(connection: &mut SqliteConnection) -> diesel::QueryResult<()> {
    connection.batch_execute(
        "PRAGMA journal_mode = WAL; \
         PRAGMA synchronous = NORMAL; \
         PRAGMA busy_timeout = 5000; \
         PRAGMA foreign_keys = ON; \
         PRAGMA wal_autocheckpoint = 1000;",
    )
}

// Kind-matched connection manager. Deliberately NOT ConnectionManager over
// the derived AnyConnection: the derived establish() tries postgres first and
// falls back to sqlite, so a postgres outage would surface as a confusing
// sqlite "unable to open database file" (or create a stray file).
pub struct AnyConnectionManager {
    db_url: String,
    kind: BackendKind,
}

impl AnyConnectionManager {
    pub fn new(db_url: &str) -> AnyConnectionManager {
        AnyConnectionManager { db_url: db_url.to_string(), kind: backend_kind(db_url) }
    }
}

impl diesel::r2d2::ManageConnection for AnyConnectionManager {
    type Connection = AnyConnection;
    type Error = diesel::r2d2::Error;

    fn connect(&self) -> Result<AnyConnection, Self::Error> {
        match self.kind {
            BackendKind::Postgres => PgConnection::establish(&self.db_url).map(AnyConnection::Postgresql),
            BackendKind::Sqlite => SqliteConnection::establish(sqlite_path(&self.db_url)).map(AnyConnection::Sqlite),
        }
        .map_err(diesel::r2d2::Error::ConnectionError)
    }

    fn is_valid(&self, connection: &mut AnyConnection) -> Result<(), Self::Error> {
        connection.ping().map_err(diesel::r2d2::Error::QueryError)
    }

    fn has_broken(&self, connection: &mut AnyConnection) -> bool {
        connection.is_broken()
    }
}

#[derive(Debug)]
struct SqlitePragmas;

impl diesel::r2d2::CustomizeConnection<AnyConnection, diesel::r2d2::Error> for SqlitePragmas {
    fn on_acquire(&self, connection: &mut AnyConnection) -> Result<(), diesel::r2d2::Error> {
        if let AnyConnection::Sqlite(connection) = connection {
            apply_sqlite_pragmas(connection).map_err(diesel::r2d2::Error::QueryError)?;
        }
        Ok(())
    }
}

pub type Pool = diesel::r2d2::Pool<AnyConnectionManager>;
pub type PooledConn = diesel::r2d2::PooledConnection<AnyConnectionManager>;

// One process-wide sqlite pool: collectors and rocket each call connect(),
// and with postgres a pool per caller is fine, but for sqlite that would
// multiply write-capable file handles. Sharing one pool bounds connections
// and keeps the pragmas uniform. If write contention (SQLITE_BUSY past the
// 5s busy_timeout) ever shows up, the fallback knob is max_size(1) here,
// which serializes writers in the pool instead of in sqlite's lock.
static SQLITE_POOL: OnceLock<Pool> = OnceLock::new();

fn build_pool(db_url: &str) -> Pool {
    let manager = AnyConnectionManager::new(db_url);
    match diesel::r2d2::Pool::builder()
        .connection_customizer(Box::new(SqlitePragmas))
        .build(manager)
    {
        Ok(pool) => pool,
        Err(e) => {
            let hint = match backend_kind(db_url) {
                BackendKind::Postgres =>
                    "check that postgres is running, the role in the URL exists, and \
                     JASPY_DB_URL survives sudo/systemd (running as a different user \
                     changes unix-socket authentication)",
                BackendKind::Sqlite =>
                    "check that the database file path is writable and its parent \
                     directory exists",
            };
            panic!("failed to connect to {}: {} — {}", redacted_db_url(db_url), e, hint);
        }
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
    match backend_kind(&db_url) {
        BackendKind::Postgres => build_pool(&db_url),
        BackendKind::Sqlite => SQLITE_POOL.get_or_init(|| build_pool(&db_url)).clone(),
    }
}

// Direct (non-pooled) connection with sqlite pragmas applied; used by
// auto-migrate and the mock seeder.
pub fn establish(db_url: &str) -> diesel::ConnectionResult<AnyConnection> {
    match backend_kind(db_url) {
        BackendKind::Postgres => Ok(AnyConnection::Postgresql(PgConnection::establish(db_url)?)),
        BackendKind::Sqlite => {
            let mut connection = SqliteConnection::establish(sqlite_path(db_url))?;
            apply_sqlite_pragmas(&mut connection)
                .map_err(diesel::ConnectionError::CouldntSetupConfiguration)?;
            Ok(AnyConnection::Sqlite(connection))
        }
    }
}

// --- migrations -------------------------------------------------------------

pub const MIGRATIONS_PG: EmbeddedMigrations = embed_migrations!("migrations");
pub const MIGRATIONS_SQLITE: EmbeddedMigrations = embed_migrations!("migrations_sqlite");

pub fn run_migrations(connection: &mut AnyConnection) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    let applied = match connection {
        AnyConnection::Postgresql(connection) => connection.run_pending_migrations(MIGRATIONS_PG)?,
        AnyConnection::Sqlite(connection) => connection.run_pending_migrations(MIGRATIONS_SQLITE)?,
    };
    Ok(applied.iter().map(|version| version.to_string()).collect())
}

pub fn has_pending_migrations(connection: &mut AnyConnection) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    match connection {
        AnyConnection::Postgresql(connection) => connection.has_pending_migration(MIGRATIONS_PG),
        AnyConnection::Sqlite(connection) => connection.has_pending_migration(MIGRATIONS_SQLITE),
    }
}

// Startup hook: apply pending embedded migrations for the configured backend.
// Default on; JASPY_AUTO_MIGRATE=false/0/no opts out (for deployments that
// manage schema externally, e.g. via the diesel CLI).
pub fn auto_migrate() {
    let enabled = env::var("JASPY_AUTO_MIGRATE")
        .map(|v| !matches!(v.to_lowercase().as_str(), "false" | "0" | "no"))
        .unwrap_or(true);
    let db_url = match env::var("JASPY_DB_URL") {
        Ok(db_url) => db_url,
        Err(_) => {
            panic!("JASPY_DB_URL env var not set!");
        }
    };
    println!("[db] backend: {} ({})", backend_kind(&db_url).as_str(), redacted_db_url(&db_url));
    if !enabled {
        println!("[db] auto-migrate disabled via JASPY_AUTO_MIGRATE");
        return;
    }
    let mut connection = establish(&db_url)
        .unwrap_or_else(|e| panic!("cannot connect to {} for migrations: {}", redacted_db_url(&db_url), e));
    match run_migrations(&mut connection) {
        Ok(applied) if applied.is_empty() => println!("[db] schema up to date"),
        Ok(applied) => println!("[db] applied migrations: {}", applied.join(", ")),
        Err(e) => panic!("migrations failed on {}: {}", redacted_db_url(&db_url), e),
    }
}

// --- rocket request guard ----------------------------------------------------

// Request guard that checks out a pooled connection for the duration of a
// request. Replaces the rocket_contrib `#[database]` fairing from Rocket 0.4.
// Derefs to AnyConnection so handlers can call diesel-2 model methods with
// `&mut *connection`.
pub struct JaspyDB(pub PooledConn);

impl Deref for JaspyDB {
    type Target = AnyConnection;
    fn deref(&self) -> &AnyConnection { &*self.0 }
}

impl DerefMut for JaspyDB {
    fn deref_mut(&mut self) -> &mut AnyConnection { &mut *self.0 }
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

#[cfg(test)]
mod tests {
    use super::*;
    use diesel::RunQueryDsl;

    #[test]
    fn redacts_password() {
        let redacted = redacted_db_url("postgres://jaspy:sup3rsecret@db.example.com:5432/jaspy");
        assert!(!redacted.contains("sup3rsecret"));
        assert!(redacted.contains("***"));
        assert!(redacted.contains("db.example.com"));
    }

    #[test]
    fn url_without_password_is_unchanged() {
        assert_eq!(redacted_db_url("postgres://jaspy@localhost/jaspy"), "postgres://jaspy@localhost/jaspy");
    }

    #[test]
    fn sqlite_urls_pass_through_redaction() {
        assert_eq!(redacted_db_url("sqlite:///var/lib/jaspy.db"), "sqlite:///var/lib/jaspy.db");
        assert_eq!(redacted_db_url("/tmp/jaspy.db"), "/tmp/jaspy.db");
        assert_eq!(redacted_db_url(":memory:"), ":memory:");
    }

    #[test]
    fn backend_detection() {
        assert_eq!(backend_kind("postgres://u@h/db"), BackendKind::Postgres);
        assert_eq!(backend_kind("postgresql://u@h/db"), BackendKind::Postgres);
        assert_eq!(backend_kind("sqlite:///var/lib/jaspy.db"), BackendKind::Sqlite);
        assert_eq!(backend_kind("sqlite:jaspy.db"), BackendKind::Sqlite);
        assert_eq!(backend_kind("/var/lib/jaspy.db"), BackendKind::Sqlite);
        assert_eq!(backend_kind(":memory:"), BackendKind::Sqlite);
    }

    #[test]
    fn sqlite_path_strips_url_prefixes() {
        assert_eq!(sqlite_path("sqlite:///var/lib/jaspy.db"), "/var/lib/jaspy.db");
        assert_eq!(sqlite_path("sqlite:jaspy.db"), "jaspy.db");
        assert_eq!(sqlite_path("/var/lib/jaspy.db"), "/var/lib/jaspy.db");
        assert_eq!(sqlite_path(":memory:"), ":memory:");
    }

    #[test]
    fn establish_sqlite_memory_applies_pragmas_and_migrations() {
        let mut connection = establish(":memory:").unwrap();

        #[derive(diesel::QueryableByName)]
        struct BusyTimeout {
            #[diesel(sql_type = diesel::sql_types::Integer)]
            timeout: i32,
        }
        let timeout: Vec<BusyTimeout> = with_backend!(&mut connection, |conn| {
            diesel::sql_query("PRAGMA busy_timeout").load(conn).unwrap()
        });
        assert_eq!(timeout[0].timeout, 5000);

        let applied = run_migrations(&mut connection).unwrap();
        assert!(!applied.is_empty(), "sqlite migrations should apply on a fresh db");
        assert_eq!(has_pending_migrations(&mut connection).unwrap(), false);
    }
}
