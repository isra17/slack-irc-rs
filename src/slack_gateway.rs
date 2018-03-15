use std;
use std::thread;
use slack_api;
use config;
use std::sync::{Arc, Mutex, RwLock};
use futures::Stream;
use futures::future::{Future, Either};
use futures::sync::mpsc::{self, UnboundedSender};
use slack;
use gateway::Gateway;
use hub::{self, Message as HubMessage, StoredInfo};

#[derive(Debug)]
pub enum Error {
    NoToken,
    InvalidCode,
    SlackApiError(slack_api::requests::Error),
    SlackAccessError(slack_api::oauth::AccessError<slack_api::requests::Error>),
    SlackError(slack::error::Error),
    AlreadyStarted,
}

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct SlackGateway {
    tx: hub::ChannelSender,
    rx: hub::Receiver,
    settings: Arc<Mutex<config::Config>>,
    hub: Arc<RwLock<StoredInfo>>,
}

#[derive(Debug)]
enum SlackEvent {
    Connect,
    Close,
    Event(slack::Event),
}

struct SlackEventHandler {
    tx: UnboundedSender<SlackEvent>,
}

impl Gateway<hub::Event> for SlackGateway {
    fn new(tx: hub::ChannelSender,
           rx: hub::Receiver,
           settings: Arc<Mutex<config::Config>>,
           hub: Arc<RwLock<StoredInfo>>)
           -> Self {
        SlackGateway {
            tx: tx,
            rx: rx,
            settings: settings,
            hub: hub,
        }
    }

    fn run(self) -> Box<Future<Item = (), Error = ()> + 'static + Send> {
        let SlackGateway { tx, rx, settings, hub } = self;
        let (slack_tx, slack_rx) = mpsc::unbounded();
        let mut gateway = SlackGatewayState::new(tx, slack_tx, settings, hub);
        match gateway.try_start_slack() {
            Ok(()) => println!("[Slack] Client started"),
            Err(e) => println!("[Slack] Client couldn't start: {:?}", e),
        };
        let gateway = Mutex::new(gateway);

        let hub_rx = rx.map(|m| Either::A(m));
        let slack_rx = slack_rx.map(|m| Either::B(m));

        Box::new(hub_rx.select(slack_rx)
            .for_each(move |v| {
                println!("[Slack] Message: {:?}", v);
                match v {
                    Either::A(hub_m) => {
                        let hub::Event { from, message, .. } = hub_m;
                        match message {
                            HubMessage::NewChannel => {
                                gateway.lock().unwrap().on_new_channel(from);
                            }
                            HubMessage::AdminCommand(cmd, args) => {
                                gateway.lock().unwrap().on_command(from, &cmd, &args);
                            }
                            _ => (),
                        }
                    }
                    Either::B(SlackEvent::Event(slack_m)) => {
                        match slack_m {
                            slack::Event::Message(msg) => {
                                match *msg {
                                    slack::Message::Standard(slack::api::MessageStandard { ref channel,
                                                                               ref text,
                                                                               ref user,
                                                                               .. }) => {
                                        gateway.lock()
                                            .unwrap()
                                            .broadcast(HubMessage::Message(user.as_ref().unwrap().clone(),
                                                                           channel.as_ref().unwrap().clone(),
                                                                           text.as_ref().unwrap().clone()));
                                    }
                                    _ => (),
                                }
                            }
                            _ => (),
                        }
                    }
                    _ => (),
                }
                Ok(())
            })
            .map_err(|e| println!("[slack] Error: {:?}", e)))
    }
}

struct SlackGatewayState {
    hub_tx: hub::ChannelSender,
    slack_tunnel: Option<UnboundedSender<SlackEvent>>,
    slack_tx: Option<slack::Sender>,
    settings: Arc<Mutex<config::Config>>,
    hub: Arc<RwLock<StoredInfo>>,
}

impl SlackGatewayState {
    pub fn new(tx: hub::ChannelSender,
               slack_tunnel: UnboundedSender<SlackEvent>,
               settings: Arc<Mutex<config::Config>>,
               hub: Arc<RwLock<StoredInfo>>)
               -> SlackGatewayState {
        SlackGatewayState {
            hub_tx: tx,
            slack_tunnel: Some(slack_tunnel),
            slack_tx: None,
            settings: settings,
            hub: hub,
        }
    }

    pub fn send_to(&self, from: hub::ChannelHandle, message: HubMessage) {
        self.hub_tx
            .send_to(from, message)
            .unwrap();
    }
    pub fn broadcast(&self, message: HubMessage) {
        self.hub_tx
            .broadcast(message)
            .unwrap();
    }

    pub fn on_new_channel(&self, from: hub::ChannelHandle) {
        if self.slack_tx.is_some() {
            return;
        }

        let client_id = {
            self.settings.lock().unwrap().get_str("slack.client_id").unwrap()
        };

        let messages =
            vec!["#### Retrieving a Slack token via OAUTH ####".into(),
                 format!("1) Paste this into a browser: \
                          https://slack.com/oauth/authorize?client_id={}&scope=client",
                         client_id),
                 "2) Select the team you wish to access from wee-slack in your browser.".into(),
                 "3) Click \"Authorize\" in the browser **IMPORTANT: the redirect will fail, \
                  this is expected**"
                     .into(),
                 "4) Copy the \"code\" portion of the URL to your clipboard".into(),
                 "5) Return to this channel and send `register [code]`".into()];
        for message in messages {
            self.send_to(from, HubMessage::AdminMessage(message));
        }
    }

    pub fn on_command<T: AsRef<str>>(&mut self, from: hub::ChannelHandle, cmd: &str, args: &[T]) {
        match cmd {
            "register" if args.len() == 1 => {
                let code = &args[0];
                match self.register_code(code.as_ref()) {
                    Err(Error::InvalidCode) => {
                        self.send_to(from,
                                     HubMessage::AdminMessage("The code appears to be invalid"
                                         .into()))
                    }
                    Err(e) => {
                        self.broadcast(HubMessage::Error(format!("[Slack] Failed to register \
                                                                  code: {:?}",
                                                                 e)))
                    }
                    Ok(token) => {
                        {
                            let hub = &mut self.hub.write().unwrap();
                            hub.set_token(&token);
                        };
                        if let Err(e) = self.start_slack(&token) {
                            self.broadcast(HubMessage::Error(format!("[Slack] Failed to \
                                                                      connect: {:?}",
                                                                     e)))
                        }
                    }

                }
            }
            _ => (),
        }
    }

    pub fn register_code(&mut self, code: &str) -> Result<String> {


        let (client_id, secret) = {
            let settings = self.settings.lock().unwrap();
            (settings.get_str("slack.client_id").unwrap(),
             settings.get_str("slack.secret").unwrap())
        };

        let client = slack_api::requests::Client::new().map_err(Error::SlackApiError)?;
        let result = slack_api::oauth::access(&client,
                                              &slack_api::oauth::AccessRequest {
                                                  client_id: &client_id,
                                                  client_secret: &secret,
                                                  code: &code,
                                                  redirect_uri: None,
                                              })
            .map_err(|e| {
                match e {
                    slack_api::oauth::AccessError::InvalidCode => Error::InvalidCode,
                    e => Error::SlackAccessError(e),
                }
            })?;
        Ok(result.access_token.unwrap())
    }

    pub fn try_start_slack(&mut self) -> Result<()> {
        let token = {
            let hub = self.hub.read().unwrap();
            hub.token()
        };
        if let Some(token) = token {
            self.start_slack(&token)
        } else {
            Err(Error::NoToken)
        }
    }

    pub fn start_slack(&mut self, token: &str) -> Result<()> {
        if self.slack_tx.is_some() {
            return Err(Error::AlreadyStarted);
        }
        let client = slack::RtmClient::login(token).map_err(Error::SlackError)?;
        self.slack_tx = Some(client.sender().clone());

        let handler_tx = self.slack_tunnel.as_ref().unwrap().clone();
        let mut handler = SlackEventHandler { tx: handler_tx };
        thread::spawn(move || client.run(&mut handler));
        Ok(())
    }
}

impl slack::EventHandler for SlackEventHandler {
    fn on_event(&mut self, _cli: &slack::RtmClient, event: slack::Event) {
        self.tx.unbounded_send(SlackEvent::Event(event)).unwrap();
    }
    fn on_close(&mut self, _cli: &slack::RtmClient) {
        self.tx.unbounded_send(SlackEvent::Close).unwrap();
    }
    fn on_connect(&mut self, _cli: &slack::RtmClient) {
        self.tx.unbounded_send(SlackEvent::Connect).unwrap();
    }
}

// pub struct SlackGatewayTask {
// slack_client: RtmClient,
// handler: SlackGatewayHandler,
// }
//

// pub struct SlackGatewayHandler {
// pub sender: Sender<Event>,
// }
//
// impl SlackGatewayManager {
// pub fn new(settings: Arc<Mutex<Config>>) -> SlackGatewayManager {
// let db_path = {
// settings.lock().unwrap().get_str("database").unwrap_or(":memory:".into())
// };
// SlackGatewayManager {
// db: Datastore::open(db_path).expect("Failed to open datastore"),
// _settings: settings,
// }
// }
//
// pub fn authenticate(&mut self,
// workspace: String,
// nick: String,
// pass: String)
// -> Option<UserInfo> {
// let user = self.db.find_user(&workspace, &nick).unwrap_or_else(|| {
// let user = UserInfo {
// id: 0,
// workspace: workspace.clone(),
// nick: nick.clone(),
// pass: hash(&pass, DEFAULT_COST).unwrap(),
// token: None,
// };
// self.db.insert_user(&user).unwrap();
// user
// });
//
// if verify(&pass, &user.pass).unwrap_or(false) {
// Some(user.clone())
// } else {
// None
// }
// }
//
// pub fn start(&mut self, user: &UserInfo) -> Result<SlackGateway, StartError> {
// if user.token.is_none() {
// return Err(StartError::MustRegister);
// }
//
// let (send, recv) = channel(0x100);
// let mut task = SlackGatewayTask::new(send, &user)
// .map_err(|e| StartError::InvalidToken(e))?;
//
// self.db.update_token(&user).unwrap();
// thread::spawn(move || task.run());
//
// Err(StartError::InvalidPass)
// }
// }
//
// impl SlackGatewayTask {
// fn new(sender: Sender<Event>,
// user: &UserInfo)
// -> Result<SlackGatewayTask, slack::error::Error> {
// Ok(SlackGatewayTask {
// slack_client: RtmClient::login(user.token.as_ref().unwrap())?,
// handler: SlackGatewayHandler { sender: sender },
// })
// }
//
// pub fn run(&mut self) {
// self.slack_client.run(&mut self.handler).unwrap()
// }
// }
//
// impl EventHandler for SlackGatewayHandler {
// fn on_event(&mut self, _cli: &RtmClient, _event: Event) {}
// fn on_close(&mut self, _cli: &RtmClient) {}
// fn on_connect(&mut self, _cli: &RtmClient) {}
// }
//
