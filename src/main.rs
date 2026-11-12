mod client;
mod connection;
mod constants;
mod protocol;
mod pty;
mod server;
mod terminal;
mod tmux;

use clap::Parser;
use client::Client;
use connection::LocalConnection;
use server::Server;
use tmux::Tmux;
use tokio::io;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Arguments {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Parser, Debug)]
enum Command {
    Server(ServerArguments),
}

#[derive(Parser, Debug)]
struct ServerArguments {}

#[tokio::main]
async fn main() -> io::Result<()> {
    tokio::fs::create_dir_all(constants::STATE_HOME.as_path()).await?;

    let arguments = Arguments::parse();
    if let Some(Command::Server(_)) = arguments.command {
        let connection = LocalConnection::default();
        let tmux = Tmux::default();
        return Server::new(connection, tmux)
            .start(tokio::signal::ctrl_c())
            .await;
    }

    let connection = LocalConnection::default();
    let client = Client::new(connection);
    client.run().await
}
