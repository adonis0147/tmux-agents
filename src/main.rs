mod client;
mod connection;
mod constants;
mod pty;
mod server;
mod tmux;

use clap::Parser;
use client::Client;
use connection::Connection;
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
    let connection = Connection::default();
    let tmux = Tmux::default();
    if let Some(Command::Server(_)) = arguments.command {
        return Server::new(connection, tmux)
            .start(tokio::signal::ctrl_c())
            .await;
    }

    let client = Client::new(connection.clone());
    if client.connect().await.is_err() {
        Server::new(connection, tmux).start_in_background()?;
    }

    Ok(())
}
