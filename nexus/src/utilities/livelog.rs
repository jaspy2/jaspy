// Live update fan-out to the web UI: named topics carrying pre-serialized
// JSON frames over the /api/v1/ws/logs/<topic> WebSocket (routes/api/v1.rs).
//
// Two kinds of topics share the machinery:
//  - log topics (e.g. "discovery"): frames are {"ts":..,"line":".."}, with a
//    bounded backlog replayed to new subscribers so history stays visible.
//  - per-device event topics ("device:<fqdn>"): frames are msgbus events
//    (models/events.rs JSON), live-only — no backlog, and nothing is even
//    serialized unless a browser is currently subscribed to that device.
//
// Publishers are plain collector threads and subscribers are async WebSocket
// handlers, hence rocket::tokio's broadcast channel: send() is synchronous and
// safe outside the runtime, recv() is awaited inside it. The registry is a
// process-wide static rather than rocket managed state so collectors can log
// without threading a handle through every call site.
use rocket::tokio::sync::broadcast;
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

const BACKLOG_LINES: usize = 500;
const CHANNEL_CAPACITY: usize = 1024;

struct Topic {
    backlog: VecDeque<String>,
    sender: broadcast::Sender<String>,
}

impl Topic {
    fn new() -> Topic {
        Topic {
            backlog: VecDeque::new(),
            sender: broadcast::channel(CHANNEL_CAPACITY).0,
        }
    }
}

static TOPICS: OnceLock<Mutex<HashMap<String, Topic>>> = OnceLock::new();

fn topics() -> &'static Mutex<HashMap<String, Topic>> {
    TOPICS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn publish_frame(topic_name: &str, frame: String, retain: bool) {
    if let Ok(mut topics) = topics().lock() {
        let topic = topics.entry(topic_name.to_string()).or_insert_with(Topic::new);
        if retain {
            topic.backlog.push_back(frame.clone());
            while topic.backlog.len() > BACKLOG_LINES {
                topic.backlog.pop_front();
            }
        }
        // send() only fails with no live subscribers, which is fine: retained
        // frames are in the backlog for whoever connects later.
        let _ = topic.sender.send(frame);
    }
}

// Log line for a log topic: timestamped, kept in the backlog.
pub fn publish(topic_name: &str, line: &str) {
    let frame = serde_json::json!({
        "ts": crate::utilities::tools::get_time(),
        "line": line,
    }).to_string();
    publish_frame(topic_name, frame, true);
}

// Msgbus event for the device it concerns: pushed to "device:<fqdn>", live
// subscribers only. Called for every event (utilities/msgbus.rs), so it must
// stay cheap when nobody is watching: bail out before serializing.
pub fn publish_event(event: &crate::models::events::Event) {
    let fqdn = match event.fqdn() {
        Some(fqdn) => fqdn,
        None => return,
    };
    let topic_name = format!("device:{}", fqdn);
    let has_subscribers = match topics().lock() {
        Ok(topics) => topics.get(&topic_name).map(|t| t.sender.receiver_count() > 0).unwrap_or(false),
        Err(_) => false,
    };
    if !has_subscribers {
        return;
    }
    if let Ok(frame) = serde_json::to_string(event) {
        publish_frame(&topic_name, frame, false);
    }
}

pub fn subscribe(topic_name: &str) -> (Vec<String>, broadcast::Receiver<String>) {
    match topics().lock() {
        Ok(mut topics) => {
            let topic = topics.entry(topic_name.to_string()).or_insert_with(Topic::new);
            (topic.backlog.iter().cloned().collect(), topic.sender.subscribe())
        },
        Err(_) => (Vec::new(), broadcast::channel(1).0.subscribe()),
    }
}
