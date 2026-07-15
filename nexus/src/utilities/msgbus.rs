use crate::models;
use std::env;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use rumqttc::{Client, MqttOptions, QoS};
extern crate serde_json;

pub struct MessageBus {
    client: Option<Client>,
    broker_addr: Option<String>,
    // None = no connection attempt concluded yet; updated by the event loop.
    connected: Arc<Mutex<Option<bool>>>,
}

impl MessageBus {
    pub fn new() -> MessageBus {
        let event_publish = match env::var("JASPY_MQTT_SERVER") {
            Ok(env_opt) => env_opt,
            Err(_) => {
                // MQTT is optional: without JASPY_MQTT_SERVER we publish nothing.
                println!("[mqtt] disabled (JASPY_MQTT_SERVER not set); events are not published to MQTT");
                return MessageBus { client: None, broker_addr: None, connected: Arc::new(Mutex::new(None)) };
            }
        };

        // Accept "host" (default port 1883) or "host:port".
        let (mqtt_host, mqtt_port) = match event_publish.rsplit_once(':') {
            Some((h, p)) => match p.parse::<u16>() {
                Ok(port) => (h.to_string(), port),
                Err(_) => (event_publish.clone(), 1883),
            },
            None => (event_publish.clone(), 1883),
        };

        let broker_addr = format!("{}:{}", mqtt_host, mqtt_port);
        let mut mqtt_options = MqttOptions::new("jaspy-nexus", mqtt_host, mqtt_port);
        mqtt_options
            .set_keep_alive(Duration::from_secs(10))
            .set_clean_session(false)
            .set_pending_throttle(Duration::from_secs(1));

        println!("[mqtt] enabled, publishing events to {}", broker_addr);
        let (client, mut connection) = Client::new(mqtt_options, 10);

        // Drive the connection event loop in a background thread. We do not
        // consume incoming publishes (nexus only produces events); iterating is
        // what keeps the client connected and transparently reconnecting.
        // Connection state is logged on transitions only — the blocking
        // Connection retries in a tight loop, so logging every failed attempt
        // would flood stdout with identical lines.
        let connected: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
        let connected_writer = connected.clone();
        let thread_broker_addr = broker_addr.clone();
        std::thread::spawn(move || {
            let mut last_connected: Option<bool> = None;
            for notification in connection.iter() {
                match &notification {
                    Ok(rumqttc::Event::Incoming(rumqttc::Packet::ConnAck(_))) => {
                        if last_connected != Some(true) {
                            println!("[mqtt] connected to {}", thread_broker_addr);
                            last_connected = Some(true);
                            if let Ok(mut connected) = connected_writer.lock() { *connected = Some(true); }
                        }
                    },
                    Ok(_) => {},
                    Err(e) => {
                        // First failure only; the retry loop alternates error
                        // causes, so keep quiet until a reconnect succeeds.
                        if last_connected != Some(false) {
                            println!("[mqtt] connection to {} failed: {} (retrying)", thread_broker_addr, e);
                            last_connected = Some(false);
                            if let Ok(mut connected) = connected_writer.lock() { *connected = Some(false); }
                        }
                    },
                }
            }
        });

        return MessageBus { client: Some(client), broker_addr: Some(broker_addr), connected: connected };
    }

    // Bus with MQTT disabled, for unit tests that need an IMDS without
    // depending on JASPY_MQTT_SERVER or spawning a connection thread.
    #[cfg(test)]
    pub fn disconnected() -> MessageBus {
        MessageBus { client: None, broker_addr: None, connected: Arc::new(Mutex::new(None)) }
    }

    // Broker address when MQTT is configured; None = disabled.
    pub fn broker(&self) -> Option<String> {
        self.broker_addr.clone()
    }

    // Some(true/false) after the first (dis)connect; None before any attempt
    // concluded or when MQTT is disabled.
    pub fn connection_status(&self) -> Option<bool> {
        self.connected.lock().ok().and_then(|c| *c)
    }

    pub fn event(self: &mut MessageBus, event: models::events::Event) {
        // Every event also goes to the per-device live-update topic for the
        // web UI, independent of whether MQTT is configured.
        crate::utilities::livelog::publish_event(&event);
        if let Some(client) = &mut self.client {
            let json_data = format!("{}", serde_json::json!(event));
            let topic = format!("jaspy/nexus/{}", event.event_type);
            // Best-effort publish, matching the previous fire-and-forget behavior.
            let _res = client.publish(topic, QoS::AtLeastOnce, false, json_data);
        }
    }
}
