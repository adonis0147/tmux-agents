use std::env;
use std::path::PathBuf;
use std::sync::LazyLock;

pub static STATE_HOME: LazyLock<PathBuf> = LazyLock::new(|| {
    env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            env::home_dir()
                .map(|home| home.join(".local/state").join(env!("CARGO_PKG_NAME")))
                .unwrap_or_else(|| PathBuf::from("/tmp"))
        })
});
