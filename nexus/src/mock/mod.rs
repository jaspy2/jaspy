// `jaspy-nexus mock`: start the full application against a small fake
// network so developers can build against the HTTP API and web UI with zero
// infrastructure. An in-process snmpbot-compatible server (snmpbot.rs) serves
// synthetic, time-animated SNMP tables for the topology in topology.rs; the
// REAL poller, entitypoller and discovery engine run against it, so the data
// visible in the API took the same path it takes in production.
//
// prepare() runs before server_main(): it provisions postgres (ephemeral
// unless JASPY_DB_URL is set), starts the fake snmpbot, seeds client
// locations + the event name, and fills in env defaults (only where unset —
// anything the developer exports wins).
pub mod pg;
pub mod seed;
pub mod snmpbot;
pub mod topology;

use std::sync::Arc;

// Dropped when server_main returns (ctrl-c): stops the ephemeral postgres /
// removes the sqlite temp dir.
enum MockDb {
    Pg(#[allow(dead_code)] pg::MockPg),
    SqliteTmp(#[allow(dead_code)] tempfile::TempDir),
    External,
}

pub struct MockGuard {
    _db: MockDb,
}

fn default_env(key: &str, value: &str) {
    if std::env::var_os(key).is_none() {
        std::env::set_var(key, value);
    }
}

pub fn prepare() -> MockGuard {
    println!("[mock] starting jaspy-nexus in mock mode: fake network, real collectors");

    // Database: developer-provided JASPY_DB_URL wins (either backend);
    // JASPY_MOCK_PG=1 spawns a throwaway postgres; the default is a sqlite
    // temp file — zero prerequisites.
    let mock_db = match std::env::var("JASPY_DB_URL") {
        Ok(db_url) => {
            println!("[mock] using external database {}", crate::db::redacted_db_url(&db_url));
            MockDb::External
        }
        Err(_) => {
            if std::env::var("JASPY_MOCK_PG").map(|v| v == "1" || v == "true").unwrap_or(false) {
                let mock_pg = pg::start();
                std::env::set_var("JASPY_DB_URL", &mock_pg.db_url);
                MockDb::Pg(mock_pg)
            } else {
                let dir = tempfile::Builder::new()
                    .prefix("jaspy-mock-sqlite-")
                    .tempdir()
                    .expect("create temp dir for mock sqlite");
                let db_url = format!("sqlite://{}", dir.path().join("jaspy.db").display());
                println!("[mock] sqlite database {} (removed on clean shutdown)", db_url);
                std::env::set_var("JASPY_DB_URL", &db_url);
                MockDb::SqliteTmp(dir)
            }
        }
    };
    // Migrate now, before the seed thread spawns: the seeder writes the event
    // setting as soon as it can connect. server_main's auto_migrate becomes a
    // no-op afterwards.
    {
        let db_url = std::env::var("JASPY_DB_URL").unwrap();
        let mut connection = crate::db::establish(&db_url)
            .unwrap_or_else(|e| panic!("[mock] cannot connect to {}: {}", crate::db::redacted_db_url(&db_url), e));
        crate::db::run_migrations(&mut connection)
            .unwrap_or_else(|e| panic!("[mock] migrations failed: {}", e));
    }

    // Fake snmpbot; must listen before the collectors' first cycle.
    let snmpbot_port: u16 = std::env::var("JASPY_MOCK_SNMPBOT_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(18286);
    let topo = Arc::new(topology::build());
    let bound_port = snmpbot::spawn(topo, snmpbot_port)
        .unwrap_or_else(|e| panic!("[mock] cannot bind fake snmpbot on port {}: {} (set JASPY_MOCK_SNMPBOT_PORT)", snmpbot_port, e));

    // Collector + discovery defaults tuned so the network is visibly alive
    // within seconds. Every value yields to a pre-set env var.
    default_env("JASPY_SNMPBOT_URL", &format!("http://127.0.0.1:{}", bound_port));
    default_env("JASPY_ENABLE_PINGER", "false"); // fake fqdns are unpingable; poller reports up/down
    default_env("JASPY_POLL_LOOP_MSECS", "5000");
    default_env("JASPY_ENTITYPOLLER_INTERVAL_MSECS", "10000");
    default_env("JASPY_VLANPOLLER_INTERVAL_MSECS", "10000");
    default_env("JASPY_LAGPOLLER_INTERVAL_MSECS", "10000");
    default_env("JASPY_IMDS_REFRESH_SECS", "2");
    default_env("JASPY_DISCOVERY_ROOT_DEVICE", topology::ROOT_DEVICE);
    default_env("JASPY_DISCOVERY_COMMUNITY", topology::COMMUNITY);
    default_env("JASPY_DISCOVERY_DNS_DOMAINS", topology::DOMAIN);
    // Periodic discovery: the first run is due immediately, crawling the fake
    // network and ingesting devices + links through the real engine.
    default_env("JASPY_DISCOVERY_INTERVAL_SECS", "300");
    default_env("JASPY_DISCOVERY_SKIP_DNS", "1");
    // Demo the Megaexcel integration: nav link + per-device "Open in Megaexcel".
    default_env("JASPY_MEGAEXCEL_URL", "https://megaexcel.arenius.fi");
    default_env("ROCKET_ADDRESS", "127.0.0.1");
    // ROCKET_PORT stays at rocket's default 8000 — the webui dev proxy target.

    // The fake fqdns have no DNS: give the device detail page deterministic
    // management addresses instead of an empty resolver answer.
    crate::routes::api::v1::install_ip_overrides(
        topology::build()
            .devices
            .iter()
            .enumerate()
            .map(|(idx, dev)| (dev.fqdn(), vec![topology::management_ip(idx)]))
            .collect(),
    );

    seed::spawn(std::env::var("JASPY_DB_URL").unwrap());

    let ui_port = std::env::var("ROCKET_PORT").unwrap_or_else(|_| "8000".to_string());
    println!("[mock] fake snmpbot:  http://127.0.0.1:{}/api/hosts/{}/tables/IF-MIB::ifTable", bound_port, topology::ROOT_DEVICE);
    println!("[mock] web UI + API:  http://127.0.0.1:{}/  (API under /api/v1, metrics at /dev/metrics)", ui_port);
    println!("[mock] the {} uplink flaps every {}s for live events", "access-hall-a-02", topology::FLAP_HALF_PERIOD_SECS);

    MockGuard { _db: mock_db }
}
