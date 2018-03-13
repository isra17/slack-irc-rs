#![feature(proc_macro, conservative_impl_trait, generators)]

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
pub mod slack_gateway;
pub mod datastore;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
