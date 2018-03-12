use std;
use std::collections::HashSet;
use std::sync::Arc;
use tokio;
use tokio::net::{TcpListener, TcpStream};
use tokio::prelude::*;
use irc;
use irc::proto::irc::IrcCodec;
use irc::proto::command::Command;
use irc::proto::message::Message;

#[derive(Default)]
struct AuthenticationInfoBuilder {
    pub cap: Option<HashSet<String>>,
    pub nick: Option<String>,
    pub pass: Option<String>,
}

#[derive(Debug)]
struct AuthenticationInfo {
    pub cap: HashSet<String>,
    pub nick: String,
    pub pass: String,
}

struct AuthenticationTask {
}

impl AuthenticationTask {
    pub fn new<R>(reader: R) -> Box<Future<Item=AuthenticationInfo, Error=()>>
    where R: Stream<Item=Message, Error=irc::error::IrcError> {
        Box::new(reader.and_then(|_| Ok(AuthenticationInfo{cap: Default::default(), nick:"".into(), pass:"".into()})).map_err(|_|()))
    }
}

impl AuthenticationInfoBuilder {
    pub fn new() -> AuthenticationInfoBuilder {
        Default::default()
    }

    pub fn process(&mut self, command: Command) {
        match command {
            Command::CAP(_,_,_,_) => (),
            _ => (),
        }
    }

    pub fn is_complete(&self) -> bool {
        self.cap.is_some() && self.nick.is_some() && self.pass.is_some()
    }

    pub fn complete(self) -> Option<AuthenticationInfo> {
        if !self.is_complete() {
            return None;
        }

        Some(AuthenticationInfo {
            cap: self.cap.unwrap(),
            nick: self.nick.unwrap(),
            pass: self.pass.unwrap(),
        })
    }
}

struct IrcGatewayState {
}

pub struct IrcGateway {
    state: Arc<IrcGatewayState>,
}

impl IrcGateway {
    pub fn new() -> Self {
        IrcGateway { state: Arc::new(IrcGatewayState {}) }
    }

    pub fn run(&self) {
        let addr = "127.0.0.1:6667".parse().unwrap();
        let listener = TcpListener::bind(&addr).unwrap();
        let state = self.state.clone();
        let server =
            listener.incoming().for_each(move |socket| state.handle_socket(socket)).map_err(|_| ());
        tokio::run(server);
    }
}

impl IrcGatewayState {
    pub fn handle_socket(&self, socket: TcpStream) -> Result<(), std::io::Error> {
        let (writer, reader) = socket.framed(IrcCodec::new("utf8").unwrap()).split();
        let task = AuthenticationTask::new(reader);
        task.and_then(|auth| {
            println!("{:?}", auth);
            Ok(())
        });
        tokio::spawn(*task);
        Ok(())
    }
}
