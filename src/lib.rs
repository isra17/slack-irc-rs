#![feature(proc_macro, conservative_impl_trait, generators)]

#[macro_use]
extern crate serde_derive;

extern crate serde;
extern crate serde_json;
extern crate failure;
extern crate futures_await as futures;
extern crate tokio;
extern crate tokio_io;
extern crate irc;
extern crate slack;
extern crate slack_api;
extern crate config;
extern crate rusqlite;
extern crate bcrypt;

pub mod irc_gateway;
pub mod irc_server;
pub mod slack_gateway;
pub mod datastore;
pub mod hub;
pub mod gateway;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
