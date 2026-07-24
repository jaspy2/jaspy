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

// Well-known topic that fans out every device event (models/events.rs),
// regardless of which device it concerns — the single-socket "all device state
// changes" feed for external integrations. Live-only, like device:<fqdn>.
pub const ALL_DEVICES_TOPIC: &str = "devices";

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

fn has_subscribers(topic_name: &str) -> bool {
    matches!(topics().lock(), Ok(topics) if
        topics.get(topic_name).map(|t| t.sender.receiver_count() > 0).unwrap_or(false))
}

// Msgbus event for the device it concerns: pushed to "device:<fqdn>" and to the
// fleet-wide ALL_DEVICES_TOPIC, live subscribers only. Called for every event
// (utilities/msgbus.rs), so it must stay cheap when nobody is watching: serialize
// only if at least one of the two topics has a subscriber.
pub fn publish_event(event: &crate::models::events::Event) {
    let fqdn = match event.fqdn() {
        Some(fqdn) => fqdn,
        None => return,
    };
    let device_topic = format!("device:{}", fqdn);
    let device_live = has_subscribers(&device_topic);
    let global_live = has_subscribers(ALL_DEVICES_TOPIC);
    if !device_live && !global_live {
        return;
    }
    if let Ok(frame) = serde_json::to_string(event) {
        if device_live {
            publish_frame(&device_topic, frame.clone(), false);
        }
        if global_live {
            publish_frame(ALL_DEVICES_TOPIC, frame, false);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::events::Event;
    use rocket::tokio::sync::broadcast::error::TryRecvError;

    // Tests share the process-global TOPICS registry and run in parallel, so
    // every assertion keys on a fqdn unique to that test: drain the receiver and
    // look for our own frame, ignoring any interleaved from other tests.
    fn received_fqdn(rx: &mut broadcast::Receiver<String>, fqdn: &str) -> bool {
        loop {
            match rx.try_recv() {
                Ok(frame) => {
                    if frame.contains(fqdn) {
                        return true;
                    }
                },
                Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => return false,
                Err(TryRecvError::Lagged(_)) => continue,
            }
        }
    }

    #[test]
    fn device_event_fans_out_to_the_global_topic() {
        let fqdn = "wildcard-global.test.example.com";
        let (_backlog, mut rx) = subscribe(ALL_DEVICES_TOPIC);
        publish_event(&Event::device_created_event(fqdn));
        assert!(received_fqdn(&mut rx, fqdn), "global topic should receive every device event");
    }

    #[test]
    fn device_only_subscriber_still_receives_its_events() {
        let fqdn = "wildcard-deviceonly.test.example.com";
        let (_backlog, mut rx) = subscribe(&format!("device:{}", fqdn));
        publish_event(&Event::device_created_event(fqdn));
        assert!(received_fqdn(&mut rx, fqdn), "per-device topic behavior is preserved");
    }

    #[test]
    fn event_with_no_fqdn_is_ignored() {
        let (_backlog, mut rx) = subscribe(ALL_DEVICES_TOPIC);
        publish_event(&Event::new_empty("somethingElse"));
        // Only assert our own sentinel never appears; other tests may publish
        // real events onto the shared global topic concurrently.
        assert!(!received_fqdn(&mut rx, "\"eventType\":\"somethingElse\""),
            "an event carrying no fqdn is never published");
    }
}
