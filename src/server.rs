use crate::connection::Connection;
use crate::constants;
use crate::pty::Pty;
use crate::tmux::Tmux;
use std::fs::{File, OpenOptions};
use tokio::{io, net::UnixListener};

pub struct Server {
    connection: Connection,
    tmux: Tmux,
}

impl Server {
    pub fn new(connection: Connection, tmux: Tmux) -> Self {
        Self { connection, tmux }
    }

    pub async fn start(
        &mut self,
        shutdown: impl std::future::Future<Output = io::Result<()>>,
    ) -> io::Result<()> {
        let _lock_file = tokio::task::spawn_blocking(Server::lock).await??;

        self.clean_up().await?;
        let listener = UnixListener::bind(&self.connection.socket_path)?;

        tokio::pin!(shutdown);
        let result = loop {
            tokio::select! {
                result = &mut shutdown => break result,
                accepted = listener.accept() => match accepted {
                    Err(error) => break Err(error),
                    Ok((stream, _)) => {
                        let tmux = self.tmux.clone();
                        tokio::spawn(async move {
                            let pty = Pty::new(stream, tmux);
                            let _ = pty.run().await;
                        });
                    }
                }
            }
        };
        result.and(self.clean_up().await)
    }

    fn lock() -> io::Result<File> {
        let path = constants::STATE_HOME
            .join(env!("CARGO_PKG_NAME"))
            .with_extension("lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;

        file.try_lock()?;
        Ok(file)
    }

    async fn clean_up(&self) -> io::Result<()> {
        self.tmux.clean_up().await?;
        match tokio::fs::remove_file(&self.connection.socket_path).await {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub fn start_in_background(&mut self) -> io::Result<()> {
        std::process::Command::new(std::env::current_exe()?)
            .arg("server")
            .spawn()?;
        Ok(())
    }
}
