use std::io;

use tokio::net::UnixStream;

use crate::connection::Connection;

pub struct Client {
    connection: Connection,
}

impl Client {
    pub fn new(connection: Connection) -> Self {
        Self { connection }
    }

    pub fn connect(&self) -> impl Future<Output = Result<UnixStream, io::Error>> {
        UnixStream::connect(self.connection.socket_path.clone())
    }
}
