use config;
use std::sync::{Arc, Mutex, RwLock};
use futures::sync::mpsc::UnboundedReceiver;
use futures::future::Future;
use hub::{ChannelSender, StoredInfo};

pub trait Gateway<T> {
    fn new(tx: ChannelSender,
           rx: UnboundedReceiver<T>,
           settings: Arc<Mutex<config::Config>>,
           hub: Arc<RwLock<StoredInfo>>)
           -> Self;
    fn run(self) -> Box<Future<Item = (), Error = ()> + 'static + Send>;
}
