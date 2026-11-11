use crate::machines::{self, SshMode};
use crate::tmux::{Tmux, literal_format, shell_quote};
use crate::workspace;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};

const HANDOFF_EXIT: i32 = 42;

#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", content = "address", rename_all = "lowercase")]
enum Destination {
    Local,
    Remote(String),
}

#[derive(Clone, Deserialize, Serialize)]
struct Context {
    version: u32,
    token: String,
    machines: Vec<String>,
    current: Destination,
}

#[derive(Deserialize, Serialize)]
struct Route {
    context: Context,
    directory: PathBuf,
    registry: Option<PathBuf>,
}

#[derive(Deserialize, Serialize)]
struct Selection {
    version: u32,
    destination: Destination,
}

pub fn run() -> Result<(), String> {
    let tmux = Tmux::named();
    let (directory, _) = workspace::prepare(&tmux)?;
    let mut machines = machines::registered().unwrap_or_else(|error| {
        eprintln!("tmux-agents: {error}; continuing with local workspaces only");
        Vec::new()
    });
    let mut destination = read_selection(&directory, &machines);
    let mut automatic = true;
    loop {
        let context = Context {
            version: 1,
            token: new_token()?,
            machines: machines.clone(),
            current: destination.clone(),
        };
        remember(&directory, &destination)?;
        let next = match &destination {
            Destination::Local => {
                let (_, session) = workspace::prepare(&tmux)?;
                attach(&tmux, &directory, &session, &context)?;
                take_request(&directory, &context.token)?
            }
            Destination::Remote(address) => match connect(address, &context, automatic) {
                Ok(next) => next,
                Err(error) => {
                    eprintln!("tmux-agents: {error}; returning to local workspace");
                    Some(Destination::Local)
                }
            },
        };
        match next {
            Some(next) => {
                machines = machines::registered().unwrap_or_else(|error| {
                    eprintln!("tmux-agents: {error}; continuing with local workspaces only");
                    Vec::new()
                });
                destination = match validate_destination(&next, &machines) {
                    Ok(()) => next,
                    Err(error) => {
                        eprintln!("tmux-agents: {error}; returning to local workspace");
                        Destination::Local
                    }
                };
                automatic = false;
            }
            None => return Ok(()),
        }
    }
}

fn read_selection(directory: &Path, machines: &[String]) -> Destination {
    let read = || -> Result<Destination, String> {
        let data = match fs::read(directory.join("selection.json")) {
            Ok(data) => data,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Destination::Local);
            }
            Err(error) => return Err(error.to_string()),
        };
        let selection: Selection =
            serde_json::from_slice(&data).map_err(|error| error.to_string())?;
        if selection.version != 1 {
            return Err("unsupported selection version".into());
        }
        validate_destination(&selection.destination, machines)?;
        Ok(selection.destination)
    };
    read().unwrap_or_else(|error| {
        eprintln!("tmux-agents: cannot resume last machine: {error}; using local workspace");
        Destination::Local
    })
}

fn validate_destination(destination: &Destination, machines: &[String]) -> Result<(), String> {
    if let Destination::Remote(address) = destination {
        machines::validate_destination(address)?;
        if !machines.contains(address) {
            return Err("requested machine is not registered".into());
        }
    }
    Ok(())
}

fn new_token() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| format!("cannot create connection token: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn remember(directory: &Path, destination: &Destination) -> Result<(), String> {
    let selection = Selection {
        version: 1,
        destination: destination.clone(),
    };
    let data = serde_json::to_vec(&selection).map_err(|error| error.to_string())?;
    let temporary = directory.join(format!("selection.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&data)?;
        file.sync_all()?;
        fs::rename(&temporary, directory.join("selection.json"))?;
        File::open(directory)?.sync_all()
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(format!("cannot save last machine: {error}"));
    }
    Ok(())
}

fn attach(tmux: &Tmux, directory: &Path, session: &str, context: &Context) -> Result<(), String> {
    install_chooser(tmux, session)?;
    let registry = if context.current == Destination::Local {
        machines::config_directory().ok()
    } else {
        None
    };
    let mut client = tmux
        .command()
        .args(["attach-session", "-t", session])
        .spawn()
        .map_err(|error| format!("cannot attach to workspace: {error}"))?;
    let option = format!("@ta_route_{}", client.id());
    let route = Route {
        context: context.clone(),
        directory: directory.to_owned(),
        registry,
    };
    let setup = serde_json::to_string(&route)
        .map_err(|error| error.to_string())
        .and_then(|route| tmux.output(&["set-option", "-g", &option, &route]));
    if let Err(error) = setup {
        let _ = client.kill();
        let _ = client.wait();
        return Err(error);
    }
    let status = client
        .wait()
        .map_err(|error| format!("cannot wait for tmux client: {error}"))?;
    let _ = tmux.output(&["set-option", "-gu", &option]);
    if let Err(error) = workspace::save_after_detach(tmux, directory) {
        eprintln!("tmux-agents: {error}");
    }
    if status.success() {
        Ok(())
    } else {
        Err(format!("tmux client exited with {status}"))
    }
}

fn install_chooser(tmux: &Tmux, session: &str) -> Result<(), String> {
    let key = tmux
        .output(&["show-options", "-gv", "@ta_workspace_key"])
        .unwrap_or_else(|_| "W".into());
    let existing = tmux
        .output(&["list-keys", "-T", "prefix", &key])
        .unwrap_or_default();
    if !existing.is_empty() && !existing.contains("--internal-chooser") {
        eprintln!(
            "tmux-agents: prefix {key} is already bound; set @ta_workspace_key to a free key to enable switching"
        );
        return Ok(());
    }
    let executable = env::current_exe().map_err(|error| error.to_string())?;
    let socket = tmux.display(session, "#{socket_path}")?;
    let callback = format!(
        "{} --internal-chooser {} #{{client_pid}} #{{q:client_name}} #{{pane_id}}",
        literal_format(&shell_quote(executable.as_os_str())?),
        literal_format(&shell_quote(socket.as_ref())?),
    );
    tmux.output(&[
        "bind-key",
        "-T",
        "prefix",
        &key,
        "run-shell",
        "-b",
        &callback,
    ])?;
    Ok(())
}

fn connect(
    address: &str,
    context: &Context,
    automatic: bool,
) -> Result<Option<Destination>, String> {
    let payload = serde_json::to_string(context).map_err(|error| error.to_string())?;
    let command = format!(
        "exec tmux-agents --internal-remote {}",
        shell_quote(payload.as_ref())?
    );
    let mode = if automatic {
        SshMode::Resume
    } else {
        SshMode::Interactive
    };
    let status = machines::ssh_command(address, mode)?
        .arg(command)
        .status()
        .map_err(|error| format!("cannot execute ssh: {error}"))?;
    if status.code() == Some(HANDOFF_EXIT) {
        // shortcut: failed retrieval can leave a small request file; add expiry if these accumulate.
        let command = format!("exec tmux-agents --internal-take-handoff {}", context.token);
        let output = machines::ssh_command(address, SshMode::Probe)?
            .arg(command)
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "cannot retrieve workspace selection from {address}"
            ));
        }
        let destination = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("invalid workspace selection: {error}"))?;
        validate_destination(&destination, &context.machines)?;
        Ok(Some(destination))
    } else if status.success() {
        Ok(None)
    } else {
        Err(format!("SSH attachment to {address} ended with {status}"))
    }
}

pub fn remote(payload: &str) -> Result<ExitCode, String> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("remote attachment requires an SSH terminal".into());
    }
    if env::var_os("TMUX").is_some_and(|value| !value.is_empty()) {
        return Err("remote attachment cannot run inside another tmux client".into());
    }
    let context: Context = serde_json::from_str(payload)
        .map_err(|error| format!("invalid remote context: {error}"))?;
    validate_context(&context)?;
    let tmux = Tmux::named();
    let (directory, session) = workspace::prepare(&tmux)?;
    attach(&tmux, &directory, &session, &context)?;
    if request_path(&directory, &context.token)?.exists() {
        Ok(ExitCode::from(HANDOFF_EXIT as u8))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

fn validate_context(context: &Context) -> Result<(), String> {
    if context.version != 1 {
        return Err(
            "incompatible workspace switching protocol; install the same build on both machines"
                .into(),
        );
    }
    validate_token(&context.token)?;
    for address in &context.machines {
        machines::validate_destination(address)?;
    }
    validate_destination(&context.current, &context.machines)
}

fn validate_token(token: &str) -> Result<(), String> {
    if token.len() != 32 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid connection token".into());
    }
    Ok(())
}

fn request_path(directory: &Path, token: &str) -> Result<PathBuf, String> {
    validate_token(token)?;
    Ok(directory.join(format!("handoff.{token}.json")))
}

fn take_request(directory: &Path, token: &str) -> Result<Option<Destination>, String> {
    let path = request_path(directory, token)?;
    let data = match fs::read(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read workspace selection: {error}")),
    };
    let destination = serde_json::from_slice(&data)
        .map_err(|error| format!("invalid workspace selection: {error}"))?;
    fs::remove_file(path).map_err(|error| error.to_string())?;
    Ok(Some(destination))
}

pub fn take_remote_request(token: &str) -> Result<(), String> {
    let destination = take_request(&workspace::state_directory()?, token)?
        .ok_or("workspace selection is missing")?;
    println!(
        "{}",
        serde_json::to_string(&destination).map_err(|error| error.to_string())?
    );
    Ok(())
}

pub fn chooser(socket: PathBuf, pid: &str, client: &str, pane: &str) -> Result<(), String> {
    let tmux = Tmux::at(socket.clone());
    let mut route = read_route(&tmux, pid)?;
    if let Some(registry) = &route.registry {
        route.context.machines = machines::registered_in(registry).unwrap_or_else(|error| {
            eprintln!("tmux-agents: {error}; showing local workspaces only");
            Vec::new()
        });
        let data = serde_json::to_string(&route).map_err(|error| error.to_string())?;
        tmux.output(&["set-option", "-g", &format!("@ta_route_{pid}"), &data])?;
    }
    let executable = env::current_exe().map_err(|error| error.to_string())?;
    let current = match &route.context.current {
        Destination::Local => "local",
        Destination::Remote(address) => address,
    };
    let mut arguments = vec![
        "display-menu".to_owned(),
        "-c".into(),
        client.into(),
        "-T".into(),
        format!("Workspaces — {current}"),
        "Sessions on this machine".into(),
        "s".into(),
        format!("choose-tree -s -t {}", tmux_id(pane, '%')?),
    ];
    let choices = std::iter::once(Destination::Local).chain(
        route
            .context
            .machines
            .iter()
            .cloned()
            .map(Destination::Remote),
    );
    for (index, destination) in choices.enumerate() {
        if destination == route.context.current {
            continue;
        }
        let label = match destination {
            Destination::Local => "local".into(),
            Destination::Remote(address) => address,
        };
        let callback = format!(
            "{} --internal-switch {} {} {} {index}",
            shell_quote(executable.as_os_str())?,
            shell_quote(socket.as_os_str())?,
            pid,
            shell_quote(client.as_ref())?,
        );
        let command = format!(
            "run-shell -b {}",
            shell_quote(literal_format(&callback).as_ref())?
        );
        // display-menu and run-shell each expand formats, so protect the command at both boundaries.
        arguments.extend([label, String::new(), literal_format(&command)]);
    }
    tmux.output(&arguments.iter().map(String::as_str).collect::<Vec<_>>())?;
    Ok(())
}

fn read_route(tmux: &Tmux, pid: &str) -> Result<Route, String> {
    if pid.is_empty() || !pid.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("invalid tmux client PID".into());
    }
    let data = tmux.output(&["show-options", "-gv", &format!("@ta_route_{pid}")])?;
    let route: Route =
        serde_json::from_str(&data).map_err(|error| format!("invalid client route: {error}"))?;
    if !route.directory.is_absolute()
        || route
            .registry
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
    {
        return Err("invalid client state or registry directory".into());
    }
    validate_context(&route.context)?;
    Ok(route)
}

fn tmux_id(value: &str, prefix: char) -> Result<&str, String> {
    let number = value.strip_prefix(prefix).ok_or("invalid tmux pane ID")?;
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("invalid tmux pane ID".into());
    }
    Ok(value)
}

pub fn switch(socket: PathBuf, pid: &str, client: &str, index: usize) -> Result<(), String> {
    let tmux = Tmux::at(socket);
    let route = read_route(&tmux, pid)?;
    // The request must belong to the client being detached, not another attached terminal.
    let actual_pid = tmux.output(&["display-message", "-p", "-c", client, "#{client_pid}"])?;
    if actual_pid != pid {
        return Err("workspace request does not match its tmux client".into());
    }
    let destination = if index == 0 {
        Destination::Local
    } else {
        Destination::Remote(
            route
                .context
                .machines
                .get(index - 1)
                .ok_or("unknown machine selection")?
                .clone(),
        )
    };
    let path = request_path(&route.directory, &route.context.token)?;
    let data = serde_json::to_vec(&destination).map_err(|error| error.to_string())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|error| format!("cannot save workspace request: {error}"))?;
    if let Err(error) = file.write_all(&data).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&path);
        return Err(format!("cannot write workspace request: {error}"));
    }
    if let Err(error) = tmux.output(&["detach-client", "-t", client]) {
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}
