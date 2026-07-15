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

pub struct MockGuard {
    // Dropped when server_main returns (ctrl-c): stops the ephemeral postgres.
    _pg: Option<pg::MockPg>,
}

fn default_env(key: &str, value: &str) {
    if std::env::var_os(key).is_none() {
        std::env::set_var(key, value);
    }
}

pub fn prepare() -> MockGuard {
    println!("[mock] starting jaspy-nexus in mock mode: fake network, real collectors");

    // Postgres: developer-provided DB wins; otherwise spawn a throwaway one.
    let ephemeral_pg = match std::env::var("JASPY_DB_URL") {
        Ok(db_url) => {
            println!("[mock] using external database {} (running migrations)", crate::db::redacted_db_url(&db_url));
            pg::run_migrations(&db_url);
            None
        }
        Err(_) => {
            let mock_pg = pg::start();
            std::env::set_var("JASPY_DB_URL", &mock_pg.db_url);
            Some(mock_pg)
        }
    };

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
    default_env("JASPY_IMDS_REFRESH_SECS", "2");
    default_env("JASPY_DISCOVERY_ROOT_DEVICE", topology::ROOT_DEVICE);
    default_env("JASPY_DISCOVERY_COMMUNITY", topology::COMMUNITY);
    default_env("JASPY_DISCOVERY_DNS_DOMAINS", topology::DOMAIN);
    // Periodic discovery: the first run is due immediately, crawling the fake
    // network and ingesting devices + links through the real engine.
    default_env("JASPY_DISCOVERY_INTERVAL_SECS", "300");
    default_env("JASPY_DISCOVERY_SKIP_DNS", "1");
    default_env("ROCKET_ADDRESS", "127.0.0.1");
    // ROCKET_PORT stays at rocket's default 8000 — the webui dev proxy target.

    seed::spawn(std::env::var("JASPY_DB_URL").unwrap());

    let ui_port = std::env::var("ROCKET_PORT").unwrap_or_else(|_| "8000".to_string());
    println!("[mock] fake snmpbot:  http://127.0.0.1:{}/api/hosts/{}/tables/IF-MIB::ifTable", bound_port, topology::ROOT_DEVICE);
    println!("[mock] web UI + API:  http://127.0.0.1:{}/  (API under /api/v1, metrics at /dev/metrics)", ui_port);
    println!("[mock] the {} uplink flaps every {}s for live events", "access-hall-a-02", topology::FLAP_HALF_PERIOD_SECS);

    MockGuard { _pg: ephemeral_pg }
}
