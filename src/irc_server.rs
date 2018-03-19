use config;
use hub::HubManager;
use irc_gateway::IrcGateway;
use std::sync::{Arc, Mutex};
use tokio;
use tokio::prelude::*;
use tokio::net::TcpListener;

pub struct IrcServer {
    state: Arc<Mutex<Shared>>,
    settings: Arc<Mutex<config::Config>>,
}

pub struct Shared {
    pub hub_manager: HubManager,
    pub settings: Arc<Mutex<config::Config>>,
}

impl IrcServer {
    pub fn new() -> Self {
        let mut settings = config::Config::default();
        settings
            .merge(config::File::with_name("settings.toml").required(false))
            .expect("Failed to parse settings.toml");
        settings
            .merge(config::File::with_name("slack.toml").required(false))
            .expect("Failed to parse slack.toml");
        println!("settings: {:?}", settings);

        let shared_settings = Arc::new(Mutex::new(settings));
        let hub_manager = HubManager::new(shared_settings.clone());
        IrcServer {
            state: Arc::new(Mutex::new(Shared {
                hub_manager: hub_manager,
                settings: shared_settings.clone(),
            })),
            settings: shared_settings,
        }
    }

    pub fn run(&self) {
        let host = {
            let settings = self.settings.lock().unwrap();
            settings.get_str("irc.listen").unwrap()
        };
        let addr = host.parse().unwrap();

        let listener = TcpListener::bind(&addr).unwrap();
        let state = self.state.clone();
        let server = listener
            .incoming()
            .for_each(move |client| {
                let gateway = IrcGateway::new(state.clone(), client);
                tokio::spawn(gateway.run().then(|result| {
                    match result {
                        Ok(_) => println!("[IRC] Task done"),
                        Err(e) => println!("[IRC] Task failed: {:?}", e),
                    }
                    Ok(())
                }));
                Ok(())
            })
            .map_err(|e| panic!("Listener error: {:?}", e));

        tokio::run(server);
    }
}
