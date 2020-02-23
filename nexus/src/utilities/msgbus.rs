use futures_util::stream::StreamExt;
use crate::models;
use std::env;
use tokio::sync::mpsc::{channel};
use rumq_client::{self, MqttOptions, QoS, eventloop, Request};
use async_std::task;
extern crate serde_json;

pub struct MessageBus {
    mqtt_channel_tx: Option<tokio::sync::mpsc::Sender<rumq_client::Request>>,
}

impl MessageBus {
    pub fn new() -> MessageBus {
        let env_opt = env::var("JASPY_MQTT_SERVER");
        let event_publish;
        let mut runtime = tokio::runtime::Runtime::new().unwrap();
        match env_opt {
            Ok(env_opt) => {
                event_publish = env_opt.clone();
            },
            Err(_) => {
                return MessageBus {
                    mqtt_channel_tx: None,
                };
            }
        }

        // TODO: fail first reconn
        let mut mqtt_options = MqttOptions::new("jaspy-nexus", event_publish, 1883);
        mqtt_options
            .set_keep_alive(10)
            .set_clean_session(false);

        let (requests_tx, requests_rx) = channel(10);

        let mut eventloop = eventloop(mqtt_options, requests_rx);
        
        runtime.block_on(async {
            let mut stream = eventloop.stream();
            while let Some(_item) = stream.next().await {
                match _item {
                    rumq_client::Notification::Connected => {
                        println!("connected to MQTT");
                        break;
                    },
                    _ => {}
                };
            }
        });

        std::thread::spawn(move || {
            runtime.block_on(async {
                let mut stream = eventloop.stream();
                while let Some(_item) = stream.next().await {
                }
            });

        });

        return MessageBus {
            mqtt_channel_tx: Some(requests_tx),
        };
    }


    pub fn event(self: &mut MessageBus, event: models::events::Event) {
        if let Some(mqtt_channel_tx) = &mut self.mqtt_channel_tx {
            let json_data = format!("{}", json!(event));
            let topic = format!("jaspy/nexus/{}", event.event_type);
            let publish = Request::Publish(rumq_client::publish(&topic, QoS::AtLeastOnce, json_data));
            task::block_on(async move {
                let _res = mqtt_channel_tx.send(publish).await;
            });
/*            if let Ok(_) = mqtt_client.publish(format!("jaspy/nexus/{}", event.event_type), QoS::AtLeastOnce, false, json_data) {
                // ..
            }*/
        }
    }
}
