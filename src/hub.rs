use bcrypt::{DEFAULT_COST, hash, verify};
use config;
use std;
use std::sync::{Arc, Mutex, RwLock};
use datastore::Datastore;
use futures::future::Future;
use futures::sync::mpsc::{self, UnboundedSender, UnboundedReceiver};
use futures::Stream;
use gateway::Gateway;
use std::collections::HashMap;
use tokio;

pub type Sender = UnboundedSender<Event>;
pub type Receiver = UnboundedReceiver<Event>;

pub type ChannelHandle = usize;
pub type HubHandle = u32;

#[derive(Debug)]
pub enum Error {
    WrongPassword,
    SendError,
    ChannelDoesNotExist(ChannelHandle),
}

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct AuthenticationInfo {
    pub workspace: String,
    pub nick: String,
    pub pass: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct HubSettings {
    pub pass: String,
    pub token: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct HubInfo {
    pub id: u32,
    pub workspace: String,
    pub nick: String,
    pub settings: HubSettings,
}

#[derive(Debug)]
pub struct StoredInfo {
    info: HubInfo,
    datastore: Arc<Mutex<Datastore>>,
}

pub struct HubManager {
    settings: Arc<Mutex<config::Config>>,
    datastore: Arc<Mutex<Datastore>>,
    hubs: HashMap<HubHandle, Arc<Mutex<Hub>>>,
}

pub struct Hub {
    _info: Arc<RwLock<StoredInfo>>,
    channels: HashMap<ChannelHandle, HubChannel>,
    tx: Sender,
}

pub struct HubChannel {
    _id: ChannelHandle,
    tx: Sender,
}

#[derive(Debug)]
pub struct ChannelSender {
    id: ChannelHandle,
    tx: Sender,
}

#[derive(Debug, Clone)]
pub enum Message {
    NewChannel,
    Message(String, String, String),
    AdminMessage(String),
    AdminCommand(String, Vec<String>),
    Error(String),
}

#[derive(Debug, Clone)]
pub struct Event {
    pub from: ChannelHandle,
    pub to: Option<ChannelHandle>,
    pub message: Message,
}

impl HubManager {
    pub fn new(settings: Arc<Mutex<config::Config>>) -> Self {
        let db_path = {
            settings.lock().unwrap().get_str("database").unwrap_or(":memory:".into())
        };
        let datastore = Datastore::open(db_path).unwrap();
        HubManager {
            settings: settings,
            hubs: Default::default(),
            datastore: Arc::new(Mutex::new(datastore)),
        }
    }

    pub fn connect<G>(&mut self, auth_info: AuthenticationInfo) -> Result<Arc<Mutex<Hub>>>
        where G: Gateway<Event>
    {
        let pass = auth_info.pass.clone();
        let hub_info = self.find_or_create(auth_info);
        if !verify(&pass, &hub_info.settings.pass).unwrap_or(false) {
            return Err(Error::WrongPassword);
        }
        let settings = self.settings.clone();
        let datastore = self.datastore.clone();

        Ok(self.hubs
            .entry(hub_info.id)
            .or_insert_with(|| {
                // Create and start Slack gateway.
                let (gateway_tx, gateway_rx) = mpsc::unbounded();
                let (hub_tx, hub_rx) = mpsc::unbounded();

                let stored_hub_info = Arc::new(RwLock::new(StoredInfo::new(datastore, hub_info)));
                let mut hub = Hub::new(stored_hub_info.clone(), hub_tx);
                let sender = hub.add_channel(gateway_tx);

                let gateway = G::new(sender, gateway_rx, settings, stored_hub_info);

                let hub = Arc::new(Mutex::new(hub));
                tokio::spawn(gateway.run());
                tokio::spawn(Hub::run(hub.clone(), hub_rx));
                hub
            })
            .clone())
    }

    pub fn find_or_create(&self, auth_info: AuthenticationInfo) -> HubInfo {
        let datastore = self.datastore.lock().unwrap();
        datastore.find_hub(&auth_info.workspace, &auth_info.nick).unwrap_or_else(|| {
            let hashed_pass = hash(&auth_info.pass, DEFAULT_COST).unwrap();
            datastore.insert_hub(&HubInfo {
                    id: 0,
                    workspace: auth_info.workspace.clone(),
                    nick: auth_info.nick.clone(),
                    settings: HubSettings {
                        pass: hashed_pass,
                        token: None,
                    },
                })
                .unwrap();
            datastore.find_hub(&auth_info.workspace, &auth_info.nick).unwrap()
        })
    }
}

impl Hub {
    fn new(info: Arc<RwLock<StoredInfo>>, tx: Sender) -> Hub {
        Hub {
            _info: info,
            channels: Default::default(),
            tx: tx,
        }
    }

    fn run(hub: Arc<Mutex<Hub>>,
           rx: Receiver)
           -> Box<Future<Item = (), Error = ()> + 'static + Send> {
        Box::new(rx.for_each(move |event| {
            let hub = &mut hub.lock().unwrap();
            let Event { from, to, message } = event;
            match to {
                Some(to) => {
                    match hub.send_to(from, to, message) {
                        Ok(()) => (),
                        Err(Error::SendError) => {
                            hub.channels.remove(&to);
                        }
                        Err(_) => (),
                    }
                }
                None => hub.broadcast(from, message),
            };
            Ok(())
        }))
    }

    pub fn add_channel(&mut self, tx: Sender) -> ChannelSender {
        let next_id = self.channels.keys().max().unwrap_or(&0) + 1;
        self.channels.insert(next_id, HubChannel::new(next_id, tx));
        self.broadcast(next_id, Message::NewChannel);
        ChannelSender {
            tx: self.tx.clone(),
            id: next_id,
        }
    }

    pub fn join(&mut self) -> Result<(ChannelSender, Receiver)> {
        let (gateway_tx, gateway_rx) = mpsc::unbounded();
        let sender = self.add_channel(gateway_tx);
        Ok((sender, gateway_rx))
    }

    pub fn broadcast(&mut self, from: ChannelHandle, message: Message) {
        let event = Event {
            from: from,
            to: None,
            message: message.clone(),
        };
        self.channels
            .retain(|&id, ref mut channel| {
                if id == from {
                    true
                } else {
                    channel.tx
                        .unbounded_send(event.clone())
                        .is_ok()
                }
            })
    }

    pub fn send_to(&mut self,
                   from: ChannelHandle,
                   to: ChannelHandle,
                   message: Message)
                   -> Result<()> {
        let channel = self.channels
            .get(&to)
            .ok_or(Error::ChannelDoesNotExist(to))?;
        channel.tx
            .unbounded_send(Event {
                from: from,
                to: Some(to),
                message: message.clone(),
            })
            .or(Err(Error::SendError))

    }
}

impl HubChannel {
    fn new(id: ChannelHandle, tx: Sender) -> HubChannel {
        HubChannel { _id: id, tx: tx }
    }
}

impl ChannelSender {
    pub fn broadcast(&self, message: Message) -> Result<()> {
        self.tx
            .unbounded_send(Event {
                from: self.id,
                to: None,
                message: message,
            })
            .or(Err(Error::SendError))
    }

    pub fn send_to(&self, to: ChannelHandle, message: Message) -> Result<()> {
        self.tx
            .unbounded_send(Event {
                from: self.id,
                to: Some(to),
                message: message,
            })
            .or(Err(Error::SendError))
    }
}

impl StoredInfo {
    pub fn new(datastore: Arc<Mutex<Datastore>>, info: HubInfo) -> StoredInfo {
        StoredInfo {
            datastore: datastore,
            info: info,
        }
    }

    pub fn set_token(&mut self, token: &str) {
        self.info.settings.token = Some(token.into());
        self.datastore.lock().unwrap().update_hub(&self.info).unwrap()
    }
}
