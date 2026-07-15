// Ephemeral PostgreSQL for `jaspy-nexus mock`: initdb into a temp dir, start
// with pg_ctl on a free localhost port, create the database and run the
// embedded Diesel migrations. Adapted from the e2e PgHarness
// (tests/common/mod.rs) — kept separate because integration tests cannot
// reach bin-crate modules.
use diesel::Connection;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

pub struct MockPg {
    // Held for its Drop (removes the directory) — after pg is stopped.
    data_dir: tempfile::TempDir,
    bindir: Option<String>,
    pub db_url: String,
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .unwrap()
        .port()
}

fn pg_cmd(bindir: &Option<String>, name: &str) -> Command {
    match bindir {
        Some(dir) => Command::new(PathBuf::from(dir).join(name)),
        None => Command::new(name),
    }
}

fn run_or_die(mut cmd: Command, what: &str) {
    match cmd.output() {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            panic!(
                "[mock] {} failed: {}\n{}",
                what,
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Err(e) => {
            panic!(
                "[mock] could not run {}: {} — install PostgreSQL (initdb/pg_ctl/createdb), \
                 set JASPY_PG_BINDIR to its bin directory, or point JASPY_DB_URL at an existing database",
                what, e
            );
        }
    }
}

pub fn run_migrations(db_url: &str) {
    let mut connection = diesel::pg::PgConnection::establish(db_url)
        .unwrap_or_else(|e| panic!("[mock] cannot connect to {} for migrations: {}", crate::db::redacted_db_url(db_url), e));
    connection
        .run_pending_migrations(MIGRATIONS)
        .unwrap_or_else(|e| panic!("[mock] migrations failed: {}", e));
}

pub fn start() -> MockPg {
    let bindir = std::env::var("JASPY_PG_BINDIR")
        .or_else(|_| std::env::var("JASPY_TEST_PG_BINDIR"))
        .ok();
    let data_dir = tempfile::Builder::new()
        .prefix("jaspy-mock-pg-")
        .tempdir()
        .expect("create temp dir for mock postgres");
    let port = free_port();

    let mut initdb = pg_cmd(&bindir, "initdb");
    initdb.arg("-U").arg("jaspy").arg("-A").arg("trust").arg("--no-sync").arg(data_dir.path());
    run_or_die(initdb, "initdb");

    // -l logfile: without it pg_ctl inherits our pipes and -w can hang.
    // Socket dir /tmp: default (the tempdir) can exceed the 103-byte limit.
    let mut pg_ctl = pg_cmd(&bindir, "pg_ctl");
    pg_ctl
        .arg("-D").arg(data_dir.path())
        .arg("-l").arg(data_dir.path().join("server.log"))
        .arg("-o").arg(format!("-p {} -c listen_addresses=127.0.0.1 -c unix_socket_directories=/tmp", port))
        .arg("-w")
        .arg("start");
    run_or_die(pg_ctl, "pg_ctl start");

    let mut createdb = pg_cmd(&bindir, "createdb");
    createdb.arg("-h").arg("127.0.0.1").arg("-p").arg(port.to_string()).arg("-U").arg("jaspy").arg("jaspy");
    run_or_die(createdb, "createdb");

    let db_url = format!("postgres://jaspy@127.0.0.1:{}/jaspy", port);
    run_migrations(&db_url);

    println!("[mock] ephemeral postgres in {} (removed on clean shutdown)", data_dir.path().display());
    MockPg { data_dir, bindir, db_url }
}

impl Drop for MockPg {
    fn drop(&mut self) {
        // Stop postgres before the TempDir field drop removes its files; a
        // SIGKILLed process skips this and leaves a stray daemon behind (the
        // data dir printed at startup identifies it: pg_ctl -D <dir> stop).
        let _ = pg_cmd(&self.bindir, "pg_ctl")
            .arg("-D").arg(self.data_dir.path())
            .arg("-m").arg("immediate")
            .arg("stop")
            .output();
    }
}
