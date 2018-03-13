
use config;
use slack_gateway;
use slack_gateway::{SlackGateway, SlackGatewayManager, StartError};
use datastore::UserInfo;
use std;
use std::sync::{Arc, Mutex};
use std::thread;
use tokio;
use tokio::io;
use tokio::net::{TcpListener, TcpStream};
use tokio::prelude::*;
use tokio_io;
use irc;
use irc::proto::irc::IrcCodec;
use irc::proto::command::Command;
use irc::proto::message::Message;
use irc::proto::response::Response;
use futures::prelude::*;
use futures::sync::oneshot;
use futures::future::Either;
use failure::Fail;
use futures;

fn irc2io(e: irc::error::IrcError) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, e.compat())
}

fn to_io<E>(e: E) -> io::Error
    where E: Into<Box<std::error::Error + Send + Sync>>
{
    std::io::Error::new(std::io::ErrorKind::Other, e)
}

pub struct IrcServer {
    state: Arc<Mutex<IrcGateway>>,
    _settings: Arc<Mutex<config::Config>>,
}

impl IrcServer {
    pub fn new() -> Self {
        let mut settings = config::Config::default();
        settings.merge(config::File::with_name("settings.toml").required(false))
            .expect("Failed to parse settings.toml");
        let shared_settings = Arc::new(Mutex::new(settings));
        let slack_manager = SlackGatewayManager::new(shared_settings.clone());
        IrcServer {
            state: Arc::new(Mutex::new(IrcGateway {
                slack_manager: slack_manager,
                settings: shared_settings.clone(),
            })),
            _settings: shared_settings,
        }
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
                        Err(e) => println!("Client error: {:?}", e),
                    }
                    Ok(())
                }));
                Ok(())
            })
            .map_err(|e| panic!("Listener error: {:?}", e));
        tokio::run(server);
    }
}


struct IrcGateway {
    slack_manager: SlackGatewayManager,
    settings: Arc<Mutex<config::Config>>,
}

type IrcStream =
    Box<futures::stream::Stream<Item = Message, Error = io::Error> + std::marker::Send>;
type IrcSink = futures::stream::SplitSink<tokio_io::codec::Framed<tokio::net::TcpStream,
                                                                  irc::proto::IrcCodec>>;


impl IrcGateway {
    #[async]
    pub fn handle_socket(shared: Arc<Mutex<Self>>, socket: TcpStream) -> io::Result<()> {
        let (writer, reader) = socket.framed(IrcCodec::new("utf8").expect("unreachable")).split();
        let mut reader = reader.map_err(irc2io);
        let (auth_info, reader) = await!(Self::read_authenticate(Box::new(reader)))?;
        let (user, writer) = await!(Self::authenticate(shared.clone(), auth_info, writer))?;
        let (slack_gateway, writer, reader) =
            await!(Self::open_slack(shared.clone(), user, writer, reader))?;

        await!(Self::link_gateways(shared, reader, writer, slack_gateway))
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
            println!("C>S: {:?}", message);
            if !auth_builder.process(&message.command) {
                commands_buffer.push(message);
            }
        }
        let auth_info = auth_builder.complete().unwrap();
        Ok((auth_info, Box::new(futures::stream::iter_ok(commands_buffer).chain(reader))))
    }

    #[async]
    fn authenticate(shared: Arc<Mutex<Self>>,
                    auth_info: AuthenticationInfo,
                    writer: IrcSink)
                    -> io::Result<(UserInfo, IrcSink)> {

        let result = {
            let slack_manager = &mut shared.lock().unwrap().slack_manager;
            let (workspace, pass) = {
                let mut nicks = auth_info.pass.splitn(2, ".");
                (String::from(nicks.next().unwrap()),
                 String::from(nicks.next()
                    .ok_or("Missing workspace from password")
                    .map_err(to_io)?))
            };
            slack_manager.authenticate(auth_info.nick, workspace, pass)
        };
        let hostname = {
            let shared = shared.lock().unwrap();
            let settings = shared.settings.lock().unwrap();
            settings.get_str("irc.host").unwrap()
        };

        if let Some(user) = result {
            let writer = IrcWriter::new(writer);
            let writer = await!(writer.rpl(hostname.clone(),
                                           Response::RPL_WELCOME,
                                           vec![user.nick.clone(),
                                                ":Welcome to the Internet Relay Network".into()]))
                ?;
            let writer = await!(writer.rpl(hostname.clone(),
                                           Response::RPL_YOURHOST,
                                           vec![user.nick.clone(),
                                                format!(":Your host is {}, running version \
                                                         slack-irc-rs-0.1",
                                                        hostname)]))
                ?;
            let writer = await!(writer.rpl(hostname.clone(),
                                           Response::RPL_CREATED,
                                           vec![user.nick.clone(),
                                                ":This server was created sometime".into()]))
                ?;
            let writer = await!(writer.rpl(hostname.clone(),
                                           Response::RPL_MYINFO,
                                           vec![user.nick.clone(),
                                                hostname.clone(),
                                                "slack-irc-rs-0.1".into(),
                                                "o".into(),
                                                "o".into()]))
                ?;
            let writer = await!(writer.rpl(hostname.clone(),
                                           Response::ERR_NOMOTD,
                                           vec!["MOTD File is missing".into()]))
                ?;
            Ok((user, writer.into()))
        } else {
            await!(writer.send(Message::new(Some(&hostname), "464", vec!["Password incorrect"], None)
                        .unwrap()))
                .map_err(irc2io)?;
            Err(to_io("Invalid password"))
        }
    }

    #[async]
    fn open_slack(shared: Arc<Mutex<Self>>,
                  user: UserInfo,
                  writer: IrcSink,
                  reader: IrcStream)
                  -> io::Result<(SlackGateway, IrcSink, IrcStream)> {
        let slack_result = await!(Self::start_slack(shared.clone(), user.clone()))?;
        match slack_result {
            Ok(g) => Ok((g, writer, reader)),
            Err(slack_gateway::StartError::MustRegister) => {
                await!(Self::register_slack(shared.clone(), user.clone(), reader, writer))
            }
            Err(slack_gateway::StartError::InvalidToken(e)) => {
                println!("Failed to connect to slack: {:?}", e);
                await!(Self::register_slack(shared.clone(), user.clone(), reader, writer))
            }
            _ => Err(to_io("Failed to start slack gateway")),
        }
    }

    #[async]
    fn register_slack(shared: Arc<Mutex<Self>>,
                      user: UserInfo,
                      reader: IrcStream,
                      writer: IrcSink)
                      -> io::Result<(SlackGateway, IrcSink, IrcStream)> {
        let botnick = {
            let shared = shared.lock().unwrap();
            let settings = shared.settings.lock().unwrap();
            settings.get_str("irc.botnick").unwrap()
        };
        let mut writer = writer;
        let mut reader = reader;
        let mut user = user;
        loop {
            writer = await!(Self::prompt_register(shared.clone(), user.clone(), writer))?;
            let result = await!(Self::read_token(shared.clone(), reader))?;
            reader = result.1;
            let code = result.0;

            let result = await!(Self::access_token(shared.clone(), code))?;
            user.token = match result {
                None => {
                    let irc_writer = IrcWriter::new(writer);
                    let irc_writer = await!(irc_writer.privmsg(botnick.clone(), user.nick.clone(), "The code appears to be invalid.".into()))?;
                    writer = irc_writer.into();
                    continue;
                },
                Some(token) => Some(token),
            };

            let result = await!(Self::start_slack(shared.clone(), user.clone()))?;
            match result {
                Err(StartError::InvalidToken(e)) => {
                    println!("Invalid token: {:?}", e);
                    let irc_writer = IrcWriter::new(writer);
                    let irc_writer = await!(irc_writer.privmsg(botnick.clone(), user.nick.clone(), "The code appears to be invalid".into()))?;
                    writer = irc_writer.into();
                    continue;
                },
                Err(e) => break Err(to_io(format!("SlackGateway Error: {:?}", e))),
                Ok(gateway) => break Ok((gateway, writer, reader)),
            }
        }
    }

    #[async]
    fn prompt_register(shared: Arc<Mutex<Self>>,
                       user: UserInfo,
                       writer: IrcSink)
                       -> io::Result<(IrcSink)> {
        let (hostname, botnick, slack_client_id) = {
            let shared = shared.lock().unwrap();
            let settings = shared.settings.lock().unwrap();
            (settings.get_str("irc.host").unwrap(),
             settings.get_str("irc.botnick").unwrap(),
             settings.get_str("slack.client_id").unwrap())
        };
        let prefix = format!("{0}!{0}@{1}", botnick, hostname);

        let writer = IrcWriter::new(writer);
        let writer = await!(writer.privmsg(prefix.clone(),
                                           user.nick.clone(),
                                           "#### Retrieving a Slack token via OAUTH ####".into()))
            ?;
        let writer = await!(writer.privmsg(prefix.clone(),
                                           user.nick.clone(),
                                           format!("1) Paste this into a browser: \
                                            https://slack.\
                                            com/oauth/authorize?client_id={}&scope=client", slack_client_id)))
            ?;
        let writer = await!(writer.privmsg(prefix.clone(),
                                           user.nick.clone(),
                                           "2) Select the team you wish to access from \
                                            wee-slack in your browser."
                                               .into()))
            ?;
        let writer = await!(writer.privmsg(prefix.clone(),
                                           user.nick.clone(),
                                           "3) Click \"Authorize\" in the browser **IMPORTANT: \
                                            the redirect will fail, this is expected**"
                                               .into()))
            ?;
        let writer = await!(writer.privmsg(prefix.clone(),
                                           user.nick.clone(),
                                           "4) Copy the \"code\" portion of the URL to your \
                                            clipboard"
                                               .into()))
            ?;
        let writer = await!(writer.privmsg(prefix.clone(),
                                           user.nick.clone(),
                                           "5) Return to this channel and send `register [code]`"
                                               .into()))
            ?;
        Ok(writer.into())
    }

    #[async]
    fn read_token(shared: Arc<Mutex<Self>>,
                  reader: IrcStream)
                  -> io::Result<(String, IrcStream)> {
        let botnick = {
            let shared = shared.lock().unwrap();
            let settings = shared.settings.lock().unwrap();
            settings.get_str("irc.botnick").unwrap()
        };

        let mut reader = reader;
        let mut commands_buffer = Vec::new();
        let token = loop {
            let result = await!(reader.into_future().map_err(|(e, _)| e))?;
            reader = result.1;
            let message = result.0.ok_or(to_io("EOF"))?;
            match message {
                Message { command: Command::PRIVMSG(ref d, ref m), .. }
                    if *d == botnick && m.starts_with("register ") => {
                    break m[9..].into();
                }
                _ => commands_buffer.push(message),
            };
        };
        Ok((token, reader))
    }

    #[async]
    fn access_token(shared: Arc<Mutex<Self>>, code: String) -> io::Result<Option<String>> {
        use slack_api::requests::{Client, Error};
        use slack_api::oauth::*;

        let (slack_client_id, slack_secret) = {
            let shared = shared.lock().unwrap();
            let settings = shared.settings.lock().unwrap();
            (settings.get_str("slack.client_id").unwrap(),
             settings.get_str("slack.secret").unwrap())
        };

        let (tx, rx) = oneshot::channel::<Result<AccessResponse, AccessError<Error>>>();


        thread::spawn(move || {
            tx.send(Client::new()
                    .map_err(AccessError::Client)
                    .and_then(|client| {
                access(&client, &AccessRequest {
                    client_id: &slack_client_id,
                    client_secret: &slack_secret,
                    code: &code,
                    redirect_uri: None,
                })
            })).unwrap();
        });

        let result = await!(rx).map_err(to_io)?;
        match result {
            Ok(r) => Ok(r.access_token),
            Err(AccessError::InvalidCode) => Ok(None),
            Err(e) => Err(to_io(e)),
        }
    }

    #[async]
    fn start_slack(shared: Arc<Mutex<Self>>, user: UserInfo) -> io::Result<Result<SlackGateway, StartError>> {
        let (tx, rx) = oneshot::channel::<Result<SlackGateway, StartError>>();


        thread::spawn(move || {
            let result = {
                let slack_manager = &mut shared.lock().unwrap().slack_manager;
                slack_manager.start(&user)
            };
            tx.send(result).unwrap();
        });

        await!(rx).map_err(to_io)
    }


    #[async]
    fn link_gateways(_shared: Arc<Mutex<Self>>,
                     reader: IrcStream,
                     _writer: IrcSink,
                     slack: SlackGateway)
                     -> io::Result<()> {
        let receiver = slack.receiver.map(|m|Either::A(m)).map_err(|_|to_io("Slack channel closed."));
        let reader = reader.map(|m|Either::B(m));

        #[async]
        for message in receiver.select(reader) {
            match message {
                Either::A(m) => println!("Slack message: {:?}", m),
                Either::B(m) => println!("IRC message: {:?}", m),
            }
        };

        Ok(())
    }
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

pub struct IrcWriter {
    writer: IrcSink,
}

impl IrcWriter {
    pub fn new(writer: IrcSink) -> IrcWriter {
        IrcWriter { writer: writer }
    }

    #[async]
    pub fn privmsg(self, from: String, to: String, msg: String) -> io::Result<IrcWriter> {
        let writer = await!(self.writer.send(Message {
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
        let writer = await!(self.writer.send(Message {
                tags: None,
                prefix: Some(hostname),
                command: Command::Response(response, args, None),
            }))
            .map_err(irc2io)?;
        Ok(IrcWriter::new(writer))
    }

    pub fn into(self) -> IrcSink {
        self.writer
    }
}
