use futures::future::Either;
use slack_gateway::SlackGateway;
use futures::prelude::*;
use irc_server::Shared;
use hub::{self, AuthenticationInfo, Receiver, ChannelSender, Message as HubMessage};
use std;
use std::sync::{Arc, Mutex};
use tokio;
use tokio::io;
use tokio::net::TcpStream;
use tokio::prelude::*;
use tokio_io;
use irc;
use irc::proto::irc::IrcCodec;
use irc::proto::message::Message as IrcMessage;
use irc::proto::command::Command;
use irc::proto::response::Response;
use failure::Fail;
use futures;

#[derive(Debug)]
pub enum Error {
    ConnectionLost,
    InvalidPassword,
    IrcError(irc::error::IrcError),
    HubError(hub::Error),
    HubClosed,
}

type Result<T> = std::result::Result<T, Error>;
type AsyncResult<T> = io::Result<std::result::Result<T, Error>>;


fn print_err(e: Error) -> io::Result<()> {
    println!("[IRC] Error: {:?}", e);
    Ok(())
}

fn irc2io(e: irc::error::IrcError) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, e.compat())
}

type IrcStream =
    Box<futures::stream::Stream<Item = IrcMessage, Error = irc::error::IrcError> + std::marker::Send>;
type IrcSink = futures::stream::SplitSink<tokio_io::codec::Framed<tokio::net::TcpStream,
                                                                  irc::proto::IrcCodec>>;

pub struct AuthInfo {
    pub workspace: String,
    pub nick: String,
    pub pass: String,
}

pub struct IrcGateway {
    shared: Arc<Mutex<Shared>>,
    irc_rx: IrcStream,
    irc_tx: IrcSink,
}

impl IrcGateway {
    pub fn new(shared: Arc<Mutex<Shared>>, client: TcpStream) -> IrcGateway {
        let (writer, reader) = client.framed(IrcCodec::new("utf8").expect("unreachable")).split();

        IrcGateway {
            shared: shared,
            irc_rx: Box::new(reader),
            irc_tx: writer,
        }
    }

    #[async]
    pub fn run(self) -> io::Result<()> {
        let IrcGateway { shared, irc_rx, irc_tx } = self;
        let mut irc_tx = irc_tx;
        let (irc_rx, auth_info) = match await!(Self::read_authenticate(irc_rx))? {
            Err(e) => return print_err(e),
            Ok(x) => x,
        };
        let nick = auth_info.nick.clone();
        let (hub_tx, hub_rx) = match Self::join_hub(shared.clone(), auth_info) {
            Err(e) => return print_err(e),
            Ok(x) => x,
        };

        irc_tx = await!(Self::welcome(shared.clone(), irc_tx, nick.clone()))?;

        let hub_rx = hub_rx.map(|m| Either::A(Ok(m)))
            .or_else(|()| Ok::<_, io::Error>(Either::A(Err(Error::HubClosed))));
        let irc_rx = irc_rx.map(|m| Either::B(Ok(m)))
            .or_else(|e| Ok::<_, io::Error>(Either::B(Err(Error::IrcError(e)))));

        let botnick = {
            let shared = shared.lock().unwrap();
            let settings = shared.settings.lock().unwrap();
            settings.get_str("irc.botnick").unwrap()
        };

        #[async]
        for message in hub_rx.select(irc_rx) {
            println!("Message: {:?}", message);
            match message {
                Either::A(Ok(hub_m)) => {
                    match hub_m.message {
                        HubMessage::Message(from, to, text) => {
                            irc_tx = await!(IrcWriter::new(irc_tx)
                                    .privmsg("isra".into(), "#general".into(), text))
                                ?
                                .into()
                        }
                        HubMessage::AdminMessage(text) => {
                            irc_tx = await!(IrcWriter::new(irc_tx)
                                    .privmsg(botnick.clone(), nick.clone(), text))
                                ?
                                .into()
                        }
                        HubMessage::Error(text) => {
                            irc_tx = await!(IrcWriter::new(irc_tx).privmsg(botnick.clone(),
                                                                           nick.clone(),
                                                                           format!("ERROR: {}",
                                                                                   text)))
                                ?
                                .into()
                        }
                        _ => (),
                    }
                }
                Either::B(Ok(irc_m)) => {
                    match irc_m.command {
                        Command::PRIVMSG(to, msg) => {
                            if to == botnick {
                                let mut split = msg.split_whitespace();
                                if let Some(cmd) = split.next() {
                                    let args = split.map(|s| s.into()).collect();
                                    hub_tx.broadcast(HubMessage::AdminCommand(cmd.into(), args))
                                        .unwrap();
                                }
                            }
                        }
                        Command::PING(..) => {
                            irc_tx =
                                await!(IrcWriter::new(irc_tx).pong(irc_m.command.clone()))?.into()
                        }
                        _ => (),
                    }
                }
                Either::A(Err(_e)) => (),
                Either::B(Err(_e)) => (),
            }
        }
        Ok(())
    }

    #[async]
    fn read_authenticate(rx: IrcStream) -> AsyncResult<(IrcStream, AuthenticationInfo)> {
        let mut auth_builder = AuthenticationInfoBuilder::new();
        let mut commands_buffer = Vec::new();
        let mut rx = rx;

        while !auth_builder.is_complete() {
            match await!(rx.into_future()) {
                Ok((Some(message), new_rx)) => {
                    rx = new_rx;
                    if !auth_builder.process(&message.command) {
                        commands_buffer.push(message);
                    }
                }
                Ok((None, _)) => return Ok(Err(Error::ConnectionLost)),
                Err((e, _)) => return Ok(Err(Error::IrcError(e))),
            };
        }
        rx = Box::new(futures::stream::iter_ok(commands_buffer).chain(rx));
        Ok(auth_builder.complete().map(|x| (rx, x)).ok_or(Error::InvalidPassword))
    }

    fn join_hub(shared: Arc<Mutex<Shared>>,
                auth_info: AuthenticationInfo)
                -> Result<(ChannelSender, Receiver)> {
        let hub_manager = &mut shared.lock().unwrap().hub_manager;
        hub_manager.connect::<SlackGateway>(auth_info)
            .and_then(|hub| hub.lock().unwrap().join())
            .map_err(Error::HubError)
    }

    #[async]
    fn welcome(shared: Arc<Mutex<Shared>>, tx: IrcSink, nick: String) -> io::Result<IrcSink> {
        let hostname = {
            let shared = shared.lock().unwrap();
            let settings = shared.settings.lock().unwrap();
            settings.get_str("irc.host").unwrap()
        };

        let writer = IrcWriter::new(tx);
        let writer = await!(writer.rpl(hostname.clone(),
                                       Response::RPL_WELCOME,
                                       vec![nick.clone(),
                                            ":Welcome to the Internet Relay Network".into()]))
            ?;
        let writer = await!(writer.rpl(hostname.clone(),
                                       Response::RPL_YOURHOST,
                                       vec![nick.clone(),
                                            format!(":Your host is {}, running version \
                                                     slack-irc-rs-0.1",
                                                    hostname)]))
            ?;
        let writer = await!(writer.rpl(hostname.clone(),
                                       Response::RPL_CREATED,
                                       vec![nick.clone(),
                                            ":This server was created sometime".into()]))
            ?;
        let writer = await!(writer.rpl(hostname.clone(),
                                       Response::RPL_MYINFO,
                                       vec![nick,
                                            hostname.clone(),
                                            "slack-irc-rs-0.1".into(),
                                            "o".into(),
                                            "o".into()]))
            ?;
        let writer = await!(writer.rpl(hostname.clone(),
                                       Response::ERR_NOMOTD,
                                       vec!["MOTD File is missing".into()]))
            ?;
        Ok(writer.into())
    }
}


#[derive(Default, Debug)]
struct AuthenticationInfoBuilder {
    pub nick: Option<String>,
    pub pass: Option<String>,
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

        let pass = self.pass.unwrap();
        let mut split = pass.splitn(2, ".");
        let workspace = String::from(split.next().unwrap());
        let pass = match split.next() {
            Some(s) => String::from(s),
            None => return None,
        };

        Some(AuthenticationInfo {
            workspace: workspace,
            nick: self.nick.unwrap(),
            pass: pass,
        })
    }
}

pub struct IrcWriter {
    writer: IrcSink,
}

impl IrcWriter {
    pub fn new(writer: IrcSink) -> IrcWriter {
        IrcWriter { writer: writer }
    }

    #[async]
    pub fn privmsg(self, from: String, to: String, msg: String) -> io::Result<IrcWriter> {
        let writer = await!(self.writer.send(IrcMessage {
                tags: None,
                prefix: Some(from),
                command: Command::PRIVMSG(to.clone(), msg),
            }))
            .map_err(irc2io)?;
        Ok(IrcWriter::new(writer))
    }

    #[async]
    pub fn rpl(self,
               hostname: String,
               response: Response,
               args: Vec<String>)
               -> io::Result<IrcWriter> {
        let writer = await!(self.writer.send(IrcMessage {
                tags: None,
                prefix: Some(hostname),
                command: Command::Response(response, args, None),
            }))
            .map_err(irc2io)?;
        Ok(IrcWriter::new(writer))
    }

    #[async]
    pub fn pong(self, ping: Command) -> io::Result<IrcWriter> {
        if let Command::PING(server1, server2) = ping {
            let writer = await!(self.writer.send(IrcMessage {
                    tags: None,
                    prefix: None,
                    command: Command::PONG(server1, server2),
                }))
                .map_err(irc2io)?;
            Ok(IrcWriter::new(writer))
        } else {
            Ok(self)
        }

    }

    pub fn into(self) -> IrcSink {
        self.writer
    }
}
