use models;
use std::env;
use rumqtt::{MqttClient, MqttOptions, QoS, ReconnectOptions};
extern crate serde_json;

pub struct MessageBus {
    mqtt_client: rumqtt::MqttClient,
}

impl MessageBus {
    pub fn new() -> MessageBus {
        let env_opt = env::var("JASPY_MQTT_SERVER");
        let event_publish;
        match env_opt {
            Ok(env_opt) => {
                event_publish = env_opt.clone();
            },
            Err(_) => {
                panic!("JASPY_EVENT_PUBLISH env var not set!");
            }
        }

        let reconnection_options = ReconnectOptions::Always(10);
        let mqtt_options = MqttOptions::new("jaspy-nexus", event_publish, 1883)
            .set_keep_alive(10)
            .set_reconnect_opts(reconnection_options)
            .set_clean_session(false);

        let (mqtt_client, _notifications) = MqttClient::start(mqtt_options).unwrap();

        return MessageBus {
            mqtt_client: mqtt_client,
        };
    }


    pub fn event(self: &mut MessageBus, event: models::events::Event) {
        let json_data = format!("{}", json!(event));
        if let Ok(_) = self.mqtt_client.publish(format!("jaspy/nexus/{}", event.event_type), QoS::AtLeastOnce, false, json_data) {
            // ..
        }
    }
}
