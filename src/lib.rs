extern crate tokio;
extern crate tokio_io;
extern crate irc;

pub mod irc_gateway;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
