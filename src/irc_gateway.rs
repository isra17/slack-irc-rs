use slack_gateway;
use slack_gateway::{SlackGateway, SlackGatewayManager};
use std;
use std::sync::{Arc, Mutex};
use tokio;
use tokio::io;
use tokio::net::{TcpListener, TcpStream};
use tokio::prelude::*;
use tokio_io;
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

struct IrcGateway {
    slack_manager: SlackGatewayManager,
}

pub struct IrcServer {
    state: Arc<Mutex<IrcGateway>>,
}

impl IrcServer {
    pub fn new() -> Self {
        let slack_manager = SlackGatewayManager::new();
        IrcServer { state: Arc::new(Mutex::new(IrcGateway { slack_manager: slack_manager })) }
    }

    pub fn run(&self) {
        let addr = "127.0.0.1:6667".parse().unwrap();
        let listener = TcpListener::bind(&addr).unwrap();
        let state = self.state.clone();
        let server = listener.incoming()
            .for_each(move |client| {
                tokio::spawn(IrcGateway::handle_socket(state.clone(), client).then(|result| {
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

type IrcStream = futures::stream::Stream<Item = Message, Error = io::Error> + std::marker::Send;
type IrcSink = futures::stream::SplitSink<tokio_io::codec::Framed<tokio::net::TcpStream,
                                                                  irc::proto::IrcCodec>>;


impl IrcGateway {
    #[async]
    pub fn handle_socket(shared: Arc<Mutex<Self>>, socket: TcpStream) -> io::Result<()> {
        let (writer, reader) = socket.framed(IrcCodec::new("utf8").expect("unreachable")).split();
        let mut writer: IrcSink = writer;
        let mut reader = reader.map_err(irc2io);
        let (auth_info, reader) = await!(Self::read_authenticate(Box::new(reader)))?;

        let (slack_gateway, writer) = {
            let (user, workspace) = {
                let mut nicks = auth_info.nick.splitn(2, ".");
                (String::from(nicks.next().unwrap()),
                 String::from(nicks.next().ok_or("Missing workspace from nick").map_err(to_io)?))
            };
            let slack_result = {
                let manager = &mut shared.lock().unwrap().slack_manager;
                manager.start(workspace, user, auth_info.pass.into())
            };

            match slack_result {
                    Ok(g) => Ok((g, writer)),
                    Err(slack_gateway::StartError::MustRegister) => {
                        await!(Self::register_slack(shared.clone(), &reader, writer))
                    }
                    Err(slack_gateway::StartError::InvalidToken(e)) => {
                        println!("Failed to connect to slack: {:?}", e);
                        await!(Self::register_slack(shared.clone(), &reader, writer))
                    }
                    _ => Err(to_io("Failed to start slack gateway")),
                }
                ?
        };

        await!(Self::link_gateways(shared, reader, writer, slack_gateway))
    }

    #[async]
    fn read_authenticate(reader: Box<IrcStream>) -> io::Result<(AuthenticationInfo, Box<IrcStream>)> {
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
                     _reader: Box<IrcStream>,
                     _writer: IrcSink,
                     _slack: SlackGateway)
                     -> io::Result<()> {
        Ok(())
    }

    #[async]
    fn register_slack(_shared: Arc<Mutex<Self>>,
                      _reader: &IrcStream,
                      writer: IrcSink)
                      -> io::Result<(SlackGateway, IrcSink)> {
        await!(writer.send(Message::new(None, "TEST", vec!["#foo"], None).unwrap()))
            .map_err(irc2io)?;

        Err(to_io("not implemented"))
    }
}
