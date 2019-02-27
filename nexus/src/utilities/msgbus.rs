use models;
use std::env;
extern crate serde_json;
extern crate zmq;

pub struct MessageBus {
    zmq_socket: zmq::Socket,
}

impl MessageBus {
    pub fn new() -> MessageBus {
        let env_opt = env::var("JASPY_EVENT_PUBLISH");
        let event_publish;
        match env_opt {
            Ok(env_opt) => {
                event_publish = env_opt.clone();
            },
            Err(_) => {
                panic!("JASPY_EVENT_PUBLISH env var not set!");
            }
        }

        let zmq_context = zmq::Context::new();
        let zmq_socket;
        if let Ok(successful_socket) = zmq_context.socket(zmq::PUB) {
            zmq_socket = successful_socket;
        } else {
            panic!("Failed to create ZMQ PUB socket!");
        }

        if let Ok(_) = zmq_socket.bind(&event_publish) {

        } else {
            panic!("Failed to bind ZMQ PUB socket!");
        }

        

        return MessageBus {
            zmq_socket: zmq_socket,
        };
    }


    pub fn event(self: &MessageBus, event: models::events::Event) {
        let json_data = format!("{}", json!(event));
        if let Ok(_) = self.zmq_socket.send_str(&event.event_type.to_uppercase(), zmq::SNDMORE) {
            if let Ok(_) = self.zmq_socket.send_str(&json_data, 0) {}
        }
    }
}
