extern crate slack;
extern crate slack_api;
use slack::{RtmClient, Event};

struct MyHandler {}

#[allow(unused_variables)]
impl slack::EventHandler for MyHandler {
    fn on_event(&mut self, cli: &RtmClient, event: Event) {
        println!("on_event(event: {:?})", event);
    }

    fn on_close(&mut self, cli: &RtmClient) {
        println!("on_close");
    }

    fn on_connect(&mut self, cli: &RtmClient) {
        println!("on_connect");
        // find the general channel id from the `StartResponse`
        let general_channel_id = cli.start_response()
            .channels
            .as_ref()
            .and_then(|channels| {
                channels.iter()
                    .find(|chan| match chan.name {
                        None => false,
                        Some(ref name) => name == "general",
                    })
            })
            .and_then(|chan| chan.id.as_ref())
            .expect("general channel not found");
        let _ = cli.sender().send_message(&general_channel_id, "Hello world! (rtm)");
        // Send a message over the real time api websocket
    }
}

fn main() {
    let api_key = "";
    let mut handler = MyHandler {};
    let r = RtmClient::login_and_run(&api_key, &mut handler);
    match r {
        Ok(_) => {}
        Err(err) => panic!("Error: {}", err),
    }
}
