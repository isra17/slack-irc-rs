extern crate slack_irc;

use slack_irc::irc_server::IrcServer;

fn main() {
    println!("Starting...");
    let irc = IrcServer::new();
    irc.run();
}
