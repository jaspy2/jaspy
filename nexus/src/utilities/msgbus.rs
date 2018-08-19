pub struct MessageBus {

}

impl MessageBus {
    pub fn new() -> MessageBus {
        return MessageBus{};
    }

    pub fn message_str(self: &MessageBus, message: &String) {
        println!("{}", message);
    }
}