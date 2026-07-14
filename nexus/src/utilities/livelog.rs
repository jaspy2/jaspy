// Live log fan-out to the web UI: named topics, each with a bounded backlog
// (replayed to new subscribers so a client connecting mid-run — or after —
// still sees the history) and a broadcast channel for live tailing over the
// /api/v1/ws/logs/<topic> WebSocket (routes/api/v1.rs).
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

#[derive(Clone, Serialize)]
pub struct LogLine {
    pub ts: f64,
    pub line: String,
}

struct Topic {
    backlog: VecDeque<LogLine>,
    sender: broadcast::Sender<LogLine>,
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

pub fn publish(topic_name: &str, line: &str) {
    let entry = LogLine {
        ts: crate::utilities::tools::get_time(),
        line: line.to_string(),
    };
    if let Ok(mut topics) = topics().lock() {
        let topic = topics.entry(topic_name.to_string()).or_insert_with(Topic::new);
        topic.backlog.push_back(entry.clone());
        while topic.backlog.len() > BACKLOG_LINES {
            topic.backlog.pop_front();
        }
        // send() only fails with no live subscribers, which is fine: the line
        // is already in the backlog for whoever connects later.
        let _ = topic.sender.send(entry);
    }
}

pub fn subscribe(topic_name: &str) -> (Vec<LogLine>, broadcast::Receiver<LogLine>) {
    match topics().lock() {
        Ok(mut topics) => {
            let topic = topics.entry(topic_name.to_string()).or_insert_with(Topic::new);
            (topic.backlog.iter().cloned().collect(), topic.sender.subscribe())
        },
        Err(_) => (Vec::new(), broadcast::channel(1).0.subscribe()),
    }
}
