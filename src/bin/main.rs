extern crate slack_irc;

use slack_irc::irc_gateway::IrcGateway;

fn main() {
    println!("Starting...");
    let irc = IrcGateway::new();
    irc.run();
}
