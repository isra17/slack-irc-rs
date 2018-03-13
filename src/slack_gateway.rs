use bcrypt::{DEFAULT_COST, hash, verify};
use config::Config;
use std::thread;
use std::sync::{Arc, Mutex};
use futures::sync::mpsc::{Receiver, Sender, channel};
use slack;
use slack::{RtmClient, Event, EventHandler};
use datastore::{Datastore, UserInfo};

#[derive(Debug)]
pub enum StartError {
    MustRegister,
    InvalidToken(slack::error::Error),
    InvalidPass,
}

pub struct SlackGatewayManager {
    _settings: Arc<Mutex<Config>>,
    db: Datastore,
}

#[derive(Debug)]
pub struct SlackGateway {
    pub receiver: Receiver<Event>,
}

pub struct SlackGatewayTask {
    slack_client: RtmClient,
    handler: SlackGatewayHandler,
}

pub struct SlackGatewayHandler {
    pub sender: Sender<Event>,
}

impl SlackGatewayManager {
    pub fn new(settings: Arc<Mutex<Config>>) -> SlackGatewayManager {
        let db_path = {
            settings.lock().unwrap().get_str("database").unwrap_or(":memory:".into())
        };
        SlackGatewayManager {
            db: Datastore::open(db_path).expect("Failed to open datastore"),
            _settings: settings,
        }
    }

    pub fn authenticate(&mut self,
                        workspace: String,
                        nick: String,
                        pass: String)
                        -> Option<UserInfo> {
        let user = self.db.find_user(&workspace, &nick).unwrap_or_else(|| {
            let user = UserInfo {
                id: 0,
                workspace: workspace.clone(),
                nick: nick.clone(),
                pass: hash(&pass, DEFAULT_COST).unwrap(),
                token: None,
            };
            self.db.insert_user(&user).unwrap();
            user
        });

        if verify(&pass, &user.pass).unwrap_or(false) {
            Some(user.clone())
        } else {
            None
        }
    }

    pub fn start(&mut self, user: &UserInfo) -> Result<SlackGateway, StartError> {
        if user.token.is_none() {
            return Err(StartError::MustRegister);
        }

        let (send, recv) = channel(0x100);
        let mut task = SlackGatewayTask::new(send, &user)
            .map_err(|e| StartError::InvalidToken(e))?;

        self.db.update_token(&user).unwrap();
        thread::spawn(move || task.run());

        Ok(SlackGateway { receiver: recv })
    }
}

impl SlackGatewayTask {
    fn new(sender: Sender<Event>,
           user: &UserInfo)
           -> Result<SlackGatewayTask, slack::error::Error> {
        Ok(SlackGatewayTask {
            slack_client: RtmClient::login(user.token.as_ref().unwrap())?,
            handler: SlackGatewayHandler { sender: sender },
        })
    }

    pub fn run(&mut self) {
        self.slack_client.run(&mut self.handler).unwrap()
    }
}

impl EventHandler for SlackGatewayHandler {
    fn on_event(&mut self, _cli: &RtmClient, _event: Event) {}
    fn on_close(&mut self, _cli: &RtmClient) {}
    fn on_connect(&mut self, _cli: &RtmClient) {}
}
