mod machine;
mod protocol;
mod supervisor;

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use protocol::{Request, Response};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() {
    if let Err(error) = run() {
        eprintln!("tmux-agents: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = env::args().skip(1);
    let mode = args
        .next()
        .ok_or("usage: tmux-agents supervisor|ensure|request METHOD [ARGS...]|machine list|register DESTINATION RELEASE_TAG")?;
    let home = state_directory()?;
    match mode.as_str() {
        "machine" => machine::run(&home, &args.collect::<Vec<_>>()),
        "supervisor" if args.len() == 0 => supervisor::run(&home),
        "ensure" if args.len() == 0 => ensure(&home),
        "request" => {
            let method = args.next().ok_or("request requires a method")?;
            let response = request(&home, &method, args.collect())?;
            match method.as_str() {
                "workspace.list" => {
                    for workspace in response.result.as_array().ok_or("invalid workspace list")? {
                        println!(
                            "{}\t{}\t{}",
                            workspace["id"].as_str().unwrap_or(""),
                            workspace["name"].as_str().unwrap_or(""),
                            if workspace["running"] == true {
                                "running"
                            } else {
                                "stopped"
                            }
                        );
                    }
                }
                "workspace.resolve" => {
                    println!(
                        "{}\n{}\n{}",
                        response.result["socket"].as_str().ok_or("missing socket")?,
                        response.result["session"]
                            .as_str()
                            .ok_or("missing session")?,
                        response.result["id"].as_str().ok_or("missing id")?
                    );
                }
                "workspace.create" => println!(
                    "{}\t{}",
                    response.result["id"].as_str().ok_or("missing id")?,
                    response.result["name"].as_str().ok_or("missing name")?
                ),
                _ => println!("{}", response.result),
            }
            Ok(())
        }
        _ => Err("usage: tmux-agents supervisor|ensure|request METHOD [ARGS...]|machine list|register DESTINATION RELEASE_TAG".into()),
    }
}

fn ensure(home: &Path) -> Result<()> {
    if request(home, "status", vec![]).is_ok() {
        return Ok(());
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(home.join("supervisor.log"))?;
    let mut command = Command::new(env::current_exe()?);
    command
        .arg("supervisor")
        .env("TMUX_AGENTS_HOME", home)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // The supervisor and tmux must outlive the invoking plugin and local tmux client.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if request(home, "status", vec![]).is_ok() {
            return Ok(());
        }
        // A concurrent startup can hold the lock before its socket is ready.
        let status = child.try_wait()?;
        if Instant::now() >= deadline {
            return Err(format!(
                "supervisor not ready (child status: {status:?}); see {}",
                home.join("supervisor.log").display()
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn request(home: &Path, method: &str, args: Vec<String>) -> Result<Response> {
    let mut socket = UnixStream::connect(home.join("control.sock"))?;
    socket.set_read_timeout(Some(Duration::from_secs(3)))?;
    socket.set_write_timeout(Some(Duration::from_secs(3)))?;
    let request = Request {
        version: protocol::VERSION,
        id: random_id()?,
        method: method.into(),
        args,
    };
    let exchange = (|| -> Result<Response> {
        protocol::write(&mut socket, &request)?;
        let response: Response = protocol::read(&mut socket)?;
        if response.version != protocol::VERSION || response.id != request.id {
            return Err("invalid protocol response".into());
        }
        Ok(response)
    })();
    let response = exchange.map_err(|error| -> Box<dyn std::error::Error> {
        if method == "workspace.create" {
            format!("{error}; creation outcome unknown: refresh workspace list before retrying")
                .into()
        } else {
            error
        }
    })?;
    if let Some(error) = &response.error {
        return Err(error.clone().into());
    }
    Ok(response)
}

fn state_directory() -> Result<PathBuf> {
    let path = match env::var_os("TMUX_AGENTS_HOME") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(env::var_os("HOME").ok_or("HOME is unset")?)
            .join(".local/state/tmux-agents"),
    };
    if !path.is_absolute() {
        return Err("TMUX_AGENTS_HOME must be absolute".into());
    }
    if !path.exists() {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&path)?;
    }
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(
            "state directory must be owned by you, not a symlink, and have permissions 0700".into(),
        );
    }
    let path = fs::canonicalize(path)?;
    let text = path.to_str().ok_or("state directory must be UTF-8")?;
    if text.chars().any(char::is_control) {
        return Err("state directory cannot contain control characters".into());
    }
    if path.as_os_str().len() > 85 {
        return Err(
            "state directory path is too long for portable Unix sockets (maximum 85 bytes)".into(),
        );
    }
    Ok(path)
}

fn random_id() -> Result<String> {
    let mut bytes = [0; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    fs::File::open(path.parent().ok_or("missing parent directory")?)?.sync_all()?;
    Ok(())
}
