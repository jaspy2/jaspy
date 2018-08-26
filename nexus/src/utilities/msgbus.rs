use models;
extern crate serde_json;

pub struct MessageBus {

}

impl MessageBus {
    pub fn new() -> MessageBus {
        return MessageBus{};
    }

    pub fn event(self: &MessageBus, event: models::events::Event) {
        println!("{}", json!(event));
    }
}