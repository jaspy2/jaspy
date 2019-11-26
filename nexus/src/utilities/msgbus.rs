use models;
use std::env;
use rumqtt::{MqttClient, MqttOptions, QoS, ReconnectOptions};
extern crate serde_json;

pub struct MessageBus {
    mqtt_client: Option<rumqtt::MqttClient>,
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
                return MessageBus {
                    mqtt_client: None
                };
            }
        }

        let reconnection_options = ReconnectOptions::AfterFirstSuccess(1);
        let mqtt_options = MqttOptions::new("jaspy-nexus", event_publish, 1883)
            .set_keep_alive(10)
            .set_reconnect_opts(reconnection_options)
            .set_clean_session(false);

        let mqtt_client;
        if let Ok(mqtt_start_result) = MqttClient::start(mqtt_options) {
            mqtt_client = mqtt_start_result.0;
        } else {
            panic!("failed initial connection to MQTT server, if MQTT server is not needed, unset JASPY_MQTT_SERVER");
        }

        return MessageBus {
            mqtt_client: Some(mqtt_client),
        };
    }


    pub fn event(self: &mut MessageBus, event: models::events::Event) {
        if let Some(mqtt_client) = &mut self.mqtt_client {
            let json_data = format!("{}", json!(event));
            if let Ok(_) = mqtt_client.publish(format!("jaspy/nexus/{}", event.event_type), QoS::AtLeastOnce, false, json_data) {
                // ..
            }
        }
    }
}
