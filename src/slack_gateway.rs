use std::thread;
use std::collections::HashMap;
use futures::sync::mpsc::{Receiver, Sender, channel};
use slack;
use slack::{RtmClient, Event, EventHandler};

#[derive(Debug)]
pub enum StartError {
    MustRegister,
    InvalidToken(slack::error::Error),
    InvalidPass,
}

#[derive(Clone)]
pub struct UserInfo {
    pub workspace: String,
    pub nick: String,
    pub pass: String,
    pub token: Option<String>,
}

pub struct SlackGatewayManager {
    users: HashMap<(String, String), UserInfo>,
}

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
    pub fn new() -> SlackGatewayManager {
        SlackGatewayManager { users: Default::default() }
    }

    pub fn authenticate(&mut self,
                        workspace: String,
                        nick: String,
                        pass: String)
                        -> Option<UserInfo> {
        let user = self.users.entry((workspace.clone(), nick.clone())).or_insert_with(|| {
            UserInfo {
                workspace: workspace.clone(),
                nick: nick.clone(),
                pass: pass.clone(),
                token: None,
            }
        });

        if user.pass == pass {
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

impl UserInfo {
    pub fn registration_url(&self) -> String {
        format!("{}", self.nick)
    }
}
