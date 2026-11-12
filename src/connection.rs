use crate::constants;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Connection {
    pub socket_path: PathBuf,
}

impl Default for Connection {
    fn default() -> Self {
        let state_home = &constants::STATE_HOME;
        Self {
            socket_path: state_home
                .join(env!("CARGO_PKG_NAME"))
                .with_extension("sock"),
        }
    }
}
