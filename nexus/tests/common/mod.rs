// Shared E2E harness: ephemeral Postgres, mock snmpbot, and a spawned real
// jaspy-nexus process. (MqttBroker lives in this module too, added below.)
#![allow(dead_code)]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use diesel::pg::PgConnection;
use diesel::prelude::*;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

/// Grab an unused TCP port on localhost.
pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn pg_cmd(name: &str) -> Command {
    match std::env::var("JASPY_TEST_PG_BINDIR") {
        Ok(dir) if !dir.is_empty() => Command::new(PathBuf::from(dir).join(name)),
        _ => Command::new(name),
    }
}

fn run(mut cmd: Command, what: &str) {
    let out = cmd.output().unwrap_or_else(|e| panic!("failed to spawn {}: {}", what, e));
    if !out.status.success() {
        panic!(
            "{} failed ({}):\nstdout: {}\nstderr: {}",
            what,
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

// ---------------------------------------------------------------------------
// Ephemeral Postgres
// ---------------------------------------------------------------------------

pub struct PgHarness {
    data_dir: tempfile::TempDir,
    port: u16,
    pub db_url: String,
}

impl PgHarness {
    pub fn start() -> PgHarness {
        let data_dir = tempfile::tempdir().unwrap();
        let data_path = data_dir.path().to_str().unwrap().to_string();
        let port = free_port();

        let mut initdb = pg_cmd("initdb");
        initdb.args(["-U", "postgres", "-A", "trust", &data_path]);
        run(initdb, "initdb");

        // Socket dir must be short (<103 bytes); use /tmp and connect via TCP.
        // `-l <logfile>` is essential: it redirects the postgres server's
        // stdout/stderr to a file so the daemon does not inherit (and hold open)
        // our captured pipes — otherwise Command::output() blocks forever on EOF.
        let server_log = data_dir.path().join("server.log");
        let mut start = pg_cmd("pg_ctl");
        start.args([
            "-D",
            &data_path,
            "-l",
            server_log.to_str().unwrap(),
            "-o",
            &format!(
                "-p {} -c listen_addresses=localhost -c unix_socket_directories=/tmp",
                port
            ),
            "-w",
            "start",
        ]);
        run(start, "pg_ctl start");

        let mut createdb = pg_cmd("createdb");
        createdb.args(["-h", "localhost", "-p", &port.to_string(), "-U", "postgres", "jaspy"]);
        run(createdb, "createdb");

        let db_url = format!("postgres://postgres@localhost:{}/jaspy", port);

        let mut conn = PgConnection::establish(&db_url)
            .unwrap_or_else(|e| panic!("connect to test db: {}", e));
        conn.run_pending_migrations(MIGRATIONS)
            .unwrap_or_else(|e| panic!("run migrations: {}", e));

        PgHarness { data_dir, port, db_url }
    }

    pub fn conn(&self) -> PgConnection {
        PgConnection::establish(&self.db_url).unwrap()
    }
}

impl Drop for PgHarness {
    fn drop(&mut self) {
        let data_path = self.data_dir.path().to_str().unwrap().to_string();
        let mut stop = pg_cmd("pg_ctl");
        stop.args(["-D", &data_path, "-m", "immediate", "-w", "stop"]);
        let _ = stop.output();
    }
}

/// Run a SELECT and get typed rows, without access to the binary crate's schema.
pub fn query_rows<T>(conn: &mut PgConnection, sql: &str) -> Vec<T>
where
    T: diesel::deserialize::QueryableByName<diesel::pg::Pg> + 'static,
{
    diesel::sql_query(sql).load::<T>(conn).unwrap()
}

// ---------------------------------------------------------------------------
// Mock snmpbot (httpmock)
// ---------------------------------------------------------------------------

pub struct SnmpbotMock {
    pub server: httpmock::MockServer,
}

impl SnmpbotMock {
    pub fn start() -> SnmpbotMock {
        SnmpbotMock { server: httpmock::MockServer::start() }
    }

    /// Base URL with no trailing slash (poller appends `/api/...`).
    pub fn url(&self) -> String {
        self.server.base_url()
    }

    /// Stub one snmpbot table for a (fqdn, community). Returns the Mock so the
    /// test can assert on hit counts.
    pub fn stub_table<'a>(
        &'a self,
        fqdn: &str,
        community: &str,
        table_id: &str,
        body: &str,
    ) -> httpmock::Mock<'a> {
        let path = format!("/api/hosts/{}/tables/{}", fqdn, table_id);
        let snmp = format!("{}@{}", community, fqdn);
        let body = body.to_string();
        self.server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(path)
                .query_param("snmp", snmp);
            then.status(200)
                .header("content-type", "application/json")
                .body(body);
        })
    }

    /// Catch-all for any other snmpbot table query (returns 404). Assert its
    /// hit count is 0 to prove nexus queried only the expected tables.
    pub fn stub_other_tables<'a>(&'a self) -> httpmock::Mock<'a> {
        self.server.mock(|when, then| {
            when.method(httpmock::Method::GET).path_matches(
                regex::Regex::new(r"^/api/hosts/.+/tables/.+$").unwrap(),
            );
            then.status(404);
        })
    }
}

// ---------------------------------------------------------------------------
// The jaspy-nexus process under test
// ---------------------------------------------------------------------------

pub struct NexusBuilder {
    db_url: String,
    snmpbot_url: Option<String>,
    mqtt_server: Option<String>,
    enable_poller: bool,
    enable_pinger: bool,
    poll_loop_msecs: u64,
}

pub struct Nexus {
    child: Child,
    log_path: PathBuf,
    pub base_url: String,
    pub client: reqwest::blocking::Client,
}

impl NexusBuilder {
    pub fn snmpbot(mut self, url: &str) -> Self {
        self.snmpbot_url = Some(url.to_string());
        self
    }
    pub fn mqtt(mut self, server: &str) -> Self {
        self.mqtt_server = Some(server.to_string());
        self
    }
    pub fn poller(mut self, on: bool) -> Self {
        self.enable_poller = on;
        self
    }
    pub fn poll_loop_msecs(mut self, ms: u64) -> Self {
        self.poll_loop_msecs = ms;
        self
    }

    pub fn start(self) -> Nexus {
        let port = free_port();
        let base_url = format!("http://127.0.0.1:{}", port);
        let log_path =
            std::env::temp_dir().join(format!("jaspy-nexus-test-{}.log", port));
        let log = std::fs::File::create(&log_path).unwrap();
        let log_err = log.try_clone().unwrap();

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_jaspy-nexus"));
        cmd.env("JASPY_DB_URL", &self.db_url)
            .env("ROCKET_ADDRESS", "127.0.0.1")
            .env("ROCKET_PORT", port.to_string())
            .env("JASPY_ENABLE_POLLER", self.enable_poller.to_string())
            .env("JASPY_ENABLE_PINGER", self.enable_pinger.to_string())
            .env("POLL_LOOP_MSECS", self.poll_loop_msecs.to_string())
            .env("JASPY_IMDS_REFRESH_SECS", "1")
            .env("JASPY_POLLER_NO_JITTER", "1")
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        if let Some(u) = &self.snmpbot_url {
            cmd.env("JASPY_SNMPBOT_URL", u);
        }
        match &self.mqtt_server {
            Some(s) => {
                cmd.env("JASPY_MQTT_SERVER", s);
            }
            None => {
                cmd.env_remove("JASPY_MQTT_SERVER");
            }
        }

        let child = cmd.spawn().expect("spawn jaspy-nexus");
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();

        let mut nexus = Nexus { child, log_path: log_path.clone(), base_url, client };
        nexus.wait_ready();
        nexus
    }
}

impl Nexus {
    pub fn builder(db_url: &str) -> NexusBuilder {
        NexusBuilder {
            db_url: db_url.to_string(),
            snmpbot_url: None,
            mqtt_server: None,
            enable_poller: false,
            enable_pinger: false,
            poll_loop_msecs: 300,
        }
    }

    fn wait_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Ok(resp) = self.client.get(self.u("/dev/device")).send() {
                if resp.status().is_success() {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        let log = std::fs::read_to_string(&self.log_path).unwrap_or_default();
        panic!("jaspy-nexus did not become ready.\n--- log ---\n{}", log);
    }

    fn u(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub fn get_text(&self, path: &str) -> String {
        self.client.get(self.u(path)).send().unwrap().text().unwrap()
    }

    pub fn get_status(&self, path: &str) -> u16 {
        self.client.get(self.u(path)).send().unwrap().status().as_u16()
    }

    /// Poll a path until it returns 2xx (e.g. wait for IMDS to learn a device).
    pub fn wait_ok(&self, path: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if (200..300).contains(&self.get_status(path)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("path {} never returned 2xx within {:?}", path, timeout);
    }

    pub fn get_json(&self, path: &str) -> serde_json::Value {
        self.client.get(self.u(path)).send().unwrap().json().unwrap()
    }

    pub fn post_json(&self, path: &str, body: &serde_json::Value) -> reqwest::blocking::Response {
        self.client.post(self.u(path)).json(body).send().unwrap()
    }

    pub fn put_json(&self, path: &str, body: &serde_json::Value) -> reqwest::blocking::Response {
        self.client.put(self.u(path)).json(body).send().unwrap()
    }

    pub fn metrics(&self) -> String {
        self.get_text("/dev/metrics")
    }
    pub fn metrics_fast(&self) -> String {
        self.get_text("/dev/metrics/fast")
    }

    /// Poll `/dev/metrics` until `needle` appears or timeout (returns the body).
    pub fn wait_for_metric(&self, needle: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            let body = self.metrics();
            if body.contains(needle) {
                return body;
            }
            if Instant::now() >= deadline {
                let log = std::fs::read_to_string(&self.log_path).unwrap_or_default();
                panic!(
                    "metric {:?} did not appear within {:?}.\n--- metrics ---\n{}\n--- log ---\n{}",
                    needle, timeout, body, log
                );
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(&self.log_path).unwrap_or_default()
    }
}

impl Drop for Nexus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.log_path);
    }
}

// ---------------------------------------------------------------------------
// Embedded MQTT broker (rumqttd) + in-process subscriber
// ---------------------------------------------------------------------------

pub struct MqttBroker {
    port: u16,
    events: Arc<Mutex<Vec<(String, String)>>>,
    // Keep the publishing/subscribing link handle alive for the broker's life.
    _link_tx: rumqttd::local::LinkTx,
}

impl MqttBroker {
    pub fn start() -> MqttBroker {
        let port = free_port();
        let config_json = serde_json::json!({
            "id": 0,
            "router": {
                "max_connections": 128,
                "max_outgoing_packet_count": 1024,
                "max_segment_size": 104857600,
                "max_segment_count": 10
            },
            "v4": {
                "test": {
                    "name": "test",
                    "listen": format!("127.0.0.1:{}", port),
                    "next_connection_delay_ms": 0,
                    "connections": {
                        "connection_timeout_ms": 5000,
                        "max_payload_size": 1048576,
                        "max_inflight_count": 256
                    }
                }
            }
        });
        let config: rumqttd::Config =
            serde_json::from_value(config_json).expect("build rumqttd config");
        let mut broker = rumqttd::Broker::new(config);
        let (mut link_tx, mut link_rx) = broker.link("e2e-collector").expect("broker link");

        std::thread::spawn(move || {
            let _ = broker.start();
        });

        link_tx.subscribe("jaspy/nexus/#").expect("subscribe");

        let events = Arc::new(Mutex::new(Vec::new()));
        let ev = events.clone();
        std::thread::spawn(move || loop {
            match link_rx.recv() {
                Ok(Some(rumqttd::Notification::Forward(fwd))) => {
                    let topic = String::from_utf8_lossy(&fwd.publish.topic).to_string();
                    let payload = String::from_utf8_lossy(&fwd.publish.payload).to_string();
                    ev.lock().unwrap().push((topic, payload));
                }
                Ok(_) => {}
                Err(_) => break,
            }
        });

        MqttBroker { port, events, _link_tx: link_tx }
    }

    /// The `host:port` string to pass as JASPY_MQTT_SERVER.
    pub fn server(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    pub fn events(&self) -> Vec<(String, String)> {
        self.events.lock().unwrap().clone()
    }

    /// Wait until a collected (topic, payload) satisfies `pred`.
    pub fn wait_for_event<F: Fn(&str, &str) -> bool>(
        &self,
        timeout: Duration,
        pred: F,
    ) -> Option<(String, String)> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(e) = self.events.lock().unwrap().iter().find(|(t, p)| pred(t, p)) {
                return Some(e.clone());
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

pub fn read_fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read fixture {}: {}", name, e))
}
