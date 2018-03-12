use slack_gateway::SlackGateway;
use std;
use std::sync::{Arc, Mutex};
use tokio;
use tokio::io;
use tokio::net::{TcpListener, TcpStream};
use tokio::prelude::*;
use irc;
use irc::proto::irc::IrcCodec;
use irc::proto::command::Command;
use irc::proto::message::Message;
use futures::prelude::*;
use failure::Fail;
use futures;

// impl From<
// .map_err(|e| {
//            std::io::Error::new(std::io::ErrorKind::Other, format!("IRC Error: {}", e))
//        })
//
//

fn irc2io(e: irc::error::IrcError) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, e.compat())
}

fn to_io<E>(e: E) -> io::Error
    where E: Into<Box<std::error::Error + Send + Sync>>
{
    std::io::Error::new(std::io::ErrorKind::Other, e)
}

#[derive(Default, Debug)]
struct AuthenticationInfoBuilder {
    pub nick: Option<String>,
    pub pass: Option<String>,
}

#[derive(Debug)]
struct AuthenticationInfo {
    pub nick: String,
    pub pass: String,
}

impl AuthenticationInfoBuilder {
    pub fn new() -> AuthenticationInfoBuilder {
        Default::default()
    }

    pub fn process(&mut self, command: &Command) -> bool {
        match *command {
            Command::NICK(ref nick) => self.nick = Some(nick.clone()),
            Command::PASS(ref pass) => self.pass = Some(pass.clone()),
            _ => return false,
        }
        true
    }

    pub fn is_complete(&self) -> bool {
        self.nick.is_some() && self.pass.is_some()
    }

    pub fn complete(self) -> Option<AuthenticationInfo> {
        if !self.is_complete() {
            return None;
        }

        Some(AuthenticationInfo {
            nick: self.nick.unwrap(),
            pass: self.pass.unwrap(),
        })
    }
}

struct IrcGatewayState {
}

pub struct IrcGateway {
    state: Arc<Mutex<IrcGatewayState>>,
}

impl IrcGateway {
    pub fn new() -> Self {
        IrcGateway { state: Arc::new(Mutex::new(IrcGatewayState {})) }
    }

    pub fn run(&self) {
        let addr = "127.0.0.1:6667".parse().unwrap();
        let listener = TcpListener::bind(&addr).unwrap();
        let state = self.state.clone();
        let server = listener.incoming()
            .for_each(move |client| {
                tokio::spawn(IrcGatewayState::handle_socket(state.clone(), client)
                    .then(|result| {
                        match result {
                            Ok(_) => println!("Client disconnected"),
                            Err(e) => println!("Error: {:?}", e),
                        }
                        Ok(())
                    }));
                Ok(())
            })
            .map_err(|e| panic!("Listener error: {:?}", e));
        tokio::run(server);
    }
}

type IrcStream =
    Box<futures::stream::Stream<Item = Message, Error = io::Error> + std::marker::Send>;
type IrcSink = Box<futures::sink::Sink<SinkItem = Message, SinkError = irc::error::IrcError>>;

impl IrcGatewayState {
    #[async]
    pub fn handle_socket(shared: Arc<Mutex<Self>>, socket: TcpStream) -> io::Result<()> {
        let (writer, reader) = socket.framed(IrcCodec::new("utf8").expect("unreachable")).split();
        let mut reader = reader.map_err(irc2io);
        let (auth_info, reader) = await!(Self::read_authenticate(Box::new(reader)))?;

        let slack_gateway = {
            let mut nicks = auth_info.nick.splitn(2, ".");
            let user = nicks.next().unwrap();
            let server = nicks.next().ok_or("Missing workspace from nick").map_err(to_io)?;
            SlackGateway::start(server, user, &auth_info.pass).map_err(to_io)?
        };

        await!(Self::link_gateways(shared, reader, Box::new(writer), slack_gateway))
    }

    #[async]
    fn read_authenticate(reader: IrcStream) -> io::Result<(AuthenticationInfo, IrcStream)> {
        let mut auth_builder = AuthenticationInfoBuilder::new();
        let mut commands_buffer = Vec::new();
        let mut reader = reader;

        while !auth_builder.is_complete() {
            let result = await!(reader.into_future()).map_err(|(e, _)| e)?;
            reader = result.1;
            let message = result.0.ok_or("EOF before Auth").map_err(to_io)?;
            if !auth_builder.process(&message.command) {
                commands_buffer.push(message);
            }
        }
        let auth_info = auth_builder.complete().unwrap();
        Ok((auth_info, Box::new(futures::stream::iter_ok(commands_buffer).chain(reader))))
    }

    #[async]
    fn link_gateways(_shared: Arc<Mutex<Self>>,
                     _reader: IrcStream,
                     _writer: IrcSink,
                     _slack: SlackGateway)
                     -> io::Result<()> {
        Ok(())
    }
}
