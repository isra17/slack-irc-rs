extern crate slack_irc;

use slack_irc::irc;

fn main() {
    println!("Starting...");
    let irc = irc::IrcGateway::new();
    irc.run();
}
