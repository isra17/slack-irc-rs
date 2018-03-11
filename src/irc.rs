use std;
use std::sync::Arc;
use tokio;
use tokio::net::{TcpListener, TcpStream};
use tokio::prelude::*;

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
    pub fn handle_socket(&self, _socket: TcpStream) -> Result<(), std::io::Error> {
        println!("New connection...");
        Ok(())
    }
}
