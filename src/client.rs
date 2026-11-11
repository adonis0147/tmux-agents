use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::Command;
use tokio::sync::{Mutex, RwLock, Semaphore, mpsc, oneshot, watch};
use tokio::time::{interval, sleep, timeout};

use crate::{Result, atomic_write, machine, protocol, random_id, transport};

type ClientResult<T> = std::result::Result<T, String>;
const RPC_TIMEOUT: Duration = Duration::from_secs(7);

#[derive(Clone, Deserialize, Serialize)]
struct Workspace {
    id: String,
    name: String,
    running: bool,
    windows: usize,
    panes: usize,
}

#[derive(Deserialize)]
struct Snapshot {
    instance: String,
    revision: u64,
    workspaces: Vec<Workspace>,
}

#[derive(Clone, Serialize)]
struct MachineView {
    destination: String,
    release_tag: String,
    state: String,
    detail: String,
    instance: String,
    revision: u64,
    workspaces: Vec<Workspace>,
}

struct Pending {
    method: String,
    args: Vec<String>,
    deadline: Instant,
    reply: oneshot::Sender<ClientResult<Value>>,
}

struct MachineHandle {
    profile: Option<machine::Machine>,
    view: Arc<RwLock<MachineView>>,
    requests: mpsc::Sender<Pending>,
    reconnect: watch::Sender<u64>,
}

#[derive(Clone, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
struct Attachment {
    machine: String,
    workspace: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Preferences {
    version: u32,
    attachments: Vec<Attachment>,
}

struct AttachmentState {
    saved: Preferences,
    pending: BTreeSet<Attachment>,
}

struct Context {
    home: PathBuf,
    socket: PathBuf,
    tmux_pid: u32,
    machines: RwLock<BTreeMap<String, Arc<MachineHandle>>>,
    attachments: Mutex<AttachmentState>,
    warning: RwLock<String>,
    connections: Arc<Semaphore>,
}

pub async fn cli(home: &Path, args: &[String]) -> Result<()> {
    let [method, socket, rest @ ..] = args else {
        return Err("usage: tmux-agents client METHOD TMUX_SOCKET [ARGS...]".into());
    };
    let socket = Path::new(socket);
    if !socket.is_absolute() {
        return Err("local tmux socket must be absolute".into());
    }
    let canonical = fs::canonicalize(socket)?;
    let socket = canonical.as_path();

    if method == "run" && rest.is_empty() {
        return run(home, socket).await.map_err(Into::into);
    }

    ensure(home, socket).await?;
    let value = match (method.as_str(), rest) {
        ("ensure", []) => return Ok(()),
        ("machines", []) => send(home, socket, "machine.list", vec![]).await?,
        ("status", []) => send(home, socket, "client.status", vec![]).await?,
        ("status", [host]) => send(home, socket, "machine.status", vec![host.clone()]).await?,
        ("workspaces", []) => send(home, socket, "workspace.list-all", vec![]).await?,
        ("workspaces", [host]) => {
            send(home, socket, "workspace.list-all", vec![host.clone()]).await?
        }
        ("request", [host, operation, parameters @ ..]) => {
            let mut forwarded = vec![host.clone(), operation.clone()];
            forwarded.extend_from_slice(parameters);
            let result = send(home, socket, "remote.request", forwarded).await?;
            return crate::print_result(operation, &result);
        }
        ("open", [host, reference]) => {
            send(
                home,
                socket,
                "attachment.open",
                vec![host.clone(), reference.clone()],
            )
            .await?
        }
        ("connect", [host]) => {
            if host != "local" && std::env::var("TMUX_AGENTS_SSH_MULTIPLEX").as_deref() != Ok("0") {
                let profile = machine::load(home)?
                    .machines
                    .into_iter()
                    .find(|profile| &profile.destination == host)
                    .ok_or("machine is not registered")?;
                transport::authenticate(home, socket, &profile).await?;
            }
            send(home, socket, "machine.reconnect", vec![host.clone()]).await?;
            wait_connected(home, socket, host).await?
        }
        ("reconnect", [host]) => {
            send(home, socket, "machine.reconnect", vec![host.clone()]).await?
        }
        _ => return Err("invalid client command or arguments".into()),
    };

    match method.as_str() {
        "ensure" | "open" | "reconnect" => (),
        "connect" => println!("Connected to {}.", rest[0]),
        "machines" => {
            for row in value.as_array().ok_or("invalid machine list")? {
                println!(
                    "{}\t{}\t{}\t{}",
                    text(row, "destination")?,
                    text(row, "release_tag")?,
                    text(row, "state")?,
                    text(row, "detail")?
                );
            }
        }
        "workspaces" => {
            for row in value.as_array().ok_or("invalid workspace list")? {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    text(row, "machine")?,
                    text(row, "id")?,
                    text(row, "name")?,
                    text(row, "state")?,
                    row["windows"],
                    row["panes"]
                );
            }
        }
        _ => println!("{value}"),
    }

    Ok(())
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("invalid {key} in helper response").into())
}

pub async fn ensure(home: &Path, socket: &Path) -> Result<()> {
    let canonical = fs::canonicalize(socket)?;
    let socket = canonical.as_path();
    let pid = tmux_pid(socket).await?;
    if helper_matches(home, socket, pid).await {
        return Ok(());
    }

    let deadline = Instant::now() + Duration::from_secs(6);
    let mut child: Option<std::process::Child> = None;
    let mut next_start = Instant::now();
    while Instant::now() < deadline {
        if helper_matches(home, socket, pid).await {
            return Ok(());
        }

        let finished = match child.as_mut() {
            Some(process) => process.try_wait()?.is_some(),
            None => true,
        };
        if finished && Instant::now() >= next_start {
            let log = OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(client_path(home, socket, ".log"))?;
            let mut command = StdCommand::new(std::env::current_exe()?);
            command
                .args(["client", "run"])
                .arg(socket)
                .env("TMUX_AGENTS_HOME", home)
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log);
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            child = Some(command.spawn()?);
            next_start = Instant::now() + Duration::from_secs(1);
        }
        sleep(Duration::from_millis(100)).await;
    }

    Err(format!(
        "client helper did not start; inspect {}",
        client_path(home, socket, ".log").display()
    )
    .into())
}

async fn helper_matches(home: &Path, socket: &Path, pid: u32) -> bool {
    let result = timeout(
        Duration::from_millis(300),
        send(home, socket, "client.status", vec![]),
    )
    .await;
    matches!(result, Ok(Ok(value)) if value["tmux_pid"].as_u64() == Some(pid as u64)
        && value["server"].as_str() == socket.to_str())
}

pub async fn send(home: &Path, socket: &Path, method: &str, args: Vec<String>) -> Result<Value> {
    let mut stream = UnixStream::connect(socket_path(home, socket)).await?;
    let request = protocol::Request {
        version: protocol::VERSION,
        id: random_id()?,
        method: method.into(),
        args,
    };
    let exchange = timeout(Duration::from_secs(12), async {
        protocol::write_async(&mut stream, &request).await?;
        let response: protocol::Response = protocol::read_async(&mut stream).await?;
        if response.version != protocol::VERSION || response.id != request.id {
            return Err("invalid helper response".into());
        }
        Ok::<_, Box<dyn std::error::Error>>(response)
    })
    .await;
    let response = match exchange {
        Ok(Ok(response)) => response,
        result => {
            let detail = match result {
                Ok(Err(error)) => error.to_string(),
                Err(_) => "helper response timed out".into(),
                _ => unreachable!(),
            };
            if method == "remote.request"
                && request
                    .args
                    .get(1)
                    .is_some_and(|method| is_mutation(method))
            {
                return Err(
                    format!("{detail}; outcome unknown: refresh state before retrying").into(),
                );
            }
            return Err(detail.into());
        }
    };
    if let Some(error) = response.error {
        return Err(error.into());
    }

    Ok(response.result)
}

pub fn socket_path(home: &Path, socket: &Path) -> PathBuf {
    client_path(home, socket, "")
}

fn client_path(home: &Path, socket: &Path, suffix: &str) -> PathBuf {
    // Stable across executable upgrades so attachment preferences survive local restarts.
    let hash = socket
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    home.join(format!("c{hash:016x}{suffix}"))
}

async fn run(home: &Path, socket: &Path) -> ClientResult<()> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(client_path(home, socket, ".lock"))
        .map_err(error_text)?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("a helper already owns this local tmux server".into());
    }

    let tmux_pid = tmux_pid(socket).await?;
    let saved = load_preferences(home, socket)?;
    let pending = saved.attachments.iter().cloned().collect();
    let context = Arc::new(Context {
        home: home.into(),
        socket: socket.into(),
        tmux_pid,
        machines: RwLock::new(BTreeMap::new()),
        attachments: Mutex::new(AttachmentState { saved, pending }),
        warning: RwLock::new(String::new()),
        connections: Arc::new(Semaphore::new(8)),
    });
    sync_registry(&context).await?;

    let path = socket_path(home, socket);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            fs::remove_file(&path).map_err(error_text)?
        }
        Ok(_) => return Err("helper socket exists but is not a socket".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.to_string()),
    }
    let listener = UnixListener::bind(&path).map_err(error_text)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(error_text)?;

    let watcher_context = context.clone();
    let watcher = tokio::spawn(async move { watch_attachments(watcher_context).await });
    let permits = Arc::new(Semaphore::new(32));
    let mut tick = interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            incoming = listener.accept() => {
                let (stream, _) = incoming.map_err(error_text)?;
                if let Ok(permit) = permits.clone().try_acquire_owned() {
                    let context = context.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        if let Err(error) = serve(context, stream).await {
                            eprintln!("client request failed: {error}");
                        }
                    });
                }
            }
            _ = tick.tick() => {
                if tmux_pid_now(&context).await != Some(context.tmux_pid) { break; }
                let warning = sync_registry(&context).await.err().unwrap_or_default();
                let mut previous = context.warning.write().await;
                if *previous != warning && !warning.is_empty() { eprintln!("{warning}"); }
                *previous = warning;
            }
        }
    }

    watcher.abort();
    let handles: Vec<_> = context.machines.write().await.values().cloned().collect();
    context.machines.write().await.clear();
    for handle in handles {
        if let Some(profile) = &handle.profile {
            transport::close_master(home, socket, profile).await;
        }
    }
    let _ = fs::remove_file(path);

    Ok(())
}

fn load_preferences(home: &Path, socket: &Path) -> ClientResult<Preferences> {
    let saved: Preferences = match fs::read(client_path(home, socket, ".json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(error_text)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Preferences {
            version: 1,
            attachments: vec![],
        },
        Err(error) => return Err(error.to_string()),
    };
    if saved.version != 1 || saved.attachments.len() > 128 {
        return Err("incompatible attachment preferences; saved file was not modified".into());
    }
    let mut seen = BTreeSet::new();
    for attachment in &saved.attachments {
        validate_host(&attachment.machine)?;
        validate_id(&attachment.workspace)?;
        if !seen.insert(attachment.clone()) {
            return Err("duplicate saved attachment".into());
        }
    }

    Ok(saved)
}

fn validate_host(host: &str) -> ClientResult<()> {
    if host == "local" {
        return Ok(());
    }
    machine::validate_destination(host).map_err(error_text)
}

pub fn validate_id(id: &str) -> ClientResult<()> {
    if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid stable workspace ID".into());
    }
    Ok(())
}

async fn sync_registry(context: &Arc<Context>) -> ClientResult<()> {
    let profiles = machine::load(&context.home).map_err(error_text)?.machines;
    let mut desired = BTreeMap::from([("local".to_string(), None)]);
    for profile in profiles {
        if profile.destination == "local" {
            return Err("SSH destination local is reserved for this computer".into());
        }
        desired.insert(profile.destination.clone(), Some(profile));
    }
    let mut handles = context.machines.write().await;
    handles.retain(|host, _| desired.contains_key(host));
    for (host, profile) in desired {
        let tag = profile
            .as_ref()
            .map_or("-", |profile| profile.release_tag.as_str());
        if handles.get(&host).is_some_and(|handle| {
            handle
                .profile
                .as_ref()
                .map_or("-", |profile| profile.release_tag.as_str())
                == tag
        }) {
            continue;
        }

        let view = Arc::new(RwLock::new(MachineView {
            destination: host.clone(),
            release_tag: tag.into(),
            state: "connecting".into(),
            detail: String::new(),
            instance: String::new(),
            revision: 0,
            workspaces: vec![],
        }));
        let (requests, receiver) = mpsc::channel(8);
        let (reconnect, wake) = watch::channel(0);
        let handle = Arc::new(MachineHandle {
            profile: profile.clone(),
            view: view.clone(),
            requests,
            reconnect,
        });
        handles.insert(host, handle);

        let context = context.clone();
        tokio::spawn(async move { machine_worker(context, profile, view, receiver, wake).await });
    }

    Ok(())
}

async fn machine_worker(
    context: Arc<Context>,
    profile: Option<machine::Machine>,
    view: Arc<RwLock<MachineView>>,
    mut requests: mpsc::Receiver<Pending>,
    mut reconnect: watch::Receiver<u64>,
) {
    let mut failures = 0;
    loop {
        set_state(&view, "connecting", "").await;
        let permit = if profile.is_some() {
            match context.connections.acquire().await {
                Ok(permit) => Some(permit),
                Err(_) => return,
            }
        } else {
            None
        };
        let connected = timeout(Duration::from_secs(12), async {
            let mut connection =
                transport::connect(&context.home, &context.socket, profile.as_ref())
                    .await
                    .map_err(error_text)?;
            let handshake = async {
                let status = connection
                    .call("status", vec![])
                    .await
                    .map_err(error_text)?;
                machine::validate_status(&status).map_err(error_text)?;
                if !status["capabilities"].as_array().is_some_and(|values| {
                    values
                        .iter()
                        .any(|value| value.as_str() == Some("state.snapshot"))
                }) {
                    return Err(
                        "supervisor upgrade required: missing state.snapshot capability"
                            .to_string(),
                    );
                }
                refresh(&mut connection, &view).await
            }
            .await;
            if let Err(error) = handshake {
                let diagnostics = connection.close().await;
                return Err(format!("{error} {diagnostics}"));
            }
            Ok::<_, String>(connection)
        })
        .await
        .unwrap_or_else(|_| Err("SSH/supervisor handshake timed out".into()));
        drop(permit);

        let error = match connected {
            Ok(mut connection) => {
                let healthy_since = Instant::now();
                let mut heartbeat = interval(Duration::from_secs(2));
                let mut last_error = String::new();
                loop {
                    tokio::select! {
                        pending = requests.recv() => {
                            let Some(pending) = pending else { connection.close().await; return; };
                            if pending.reply.is_closed() { continue; }
                            if Instant::now() >= pending.deadline {
                                let _ = pending.reply.send(Err("request expired before dispatch".into()));
                                continue;
                            }
                            let (mut failed, mut result) = {
                                let outcome = connection.call(&pending.method, pending.args).await;
                                let failed = outcome.as_ref().err().is_some_and(|error| {
                                    error.downcast_ref::<transport::ServerError>().is_none()
                                });
                                (failed, outcome.map_err(error_text))
                            };
                            if let Ok(value) = &result {
                                if pending.method == "state.snapshot" {
                                    if let Err(error) = apply_snapshot(value.clone(), &view).await {
                                        result = Err(error);
                                        failed = true;
                                    }
                                } else if is_mutation(&pending.method)
                                    && let Err(error) = refresh(&mut connection, &view).await {
                                    last_error = error;
                                    failed = true;
                                }
                            }
                            if failed && let Err(error) = &result {
                                last_error = error.clone();
                            }
                            let _ = pending.reply.send(result);
                            if failed { break; }
                        }
                        changed = reconnect.changed() => {
                            if changed.is_err() { connection.close().await; return; }
                            failures = 0;
                            break;
                        }
                        _ = heartbeat.tick() => {
                            if let Err(error) = refresh(&mut connection, &view).await {
                                last_error = error;
                                break;
                            }
                        }
                    }
                }
                if healthy_since.elapsed() >= Duration::from_secs(15) {
                    failures = 0;
                }
                let diagnostics = connection.close().await;
                format!("{last_error} {diagnostics}")
            }
            Err(error) => error,
        };
        let error = clean_detail(&error);
        let attention = needs_attention(&error);
        set_state(
            &view,
            if attention {
                "needs-attention"
            } else {
                "reconnecting"
            },
            &error,
        )
        .await;
        while let Ok(pending) = requests.try_recv() {
            let _ = pending
                .reply
                .send(Err(format!("machine is not synchronized: {error}")));
        }

        let seed = random_id()
            .ok()
            .and_then(|id| u64::from_str_radix(&id[..8], 16).ok())
            .unwrap_or(0);
        let delay = reconnect_delay(failures, seed);
        failures = failures.saturating_add(1);
        tokio::select! {
            _ = sleep(delay), if !attention => (),
            changed = reconnect.changed() => {
                if changed.is_err() { return; }
                failures = 0;
            }
            pending = requests.recv() => {
                let Some(pending) = pending else { return; };
                let _ = pending.reply.send(Err(format!("machine is not synchronized: {error}")));
            }
        }
    }
}

async fn refresh(
    connection: &mut transport::Connection,
    view: &RwLock<MachineView>,
) -> ClientResult<()> {
    let value = connection
        .call("state.snapshot", vec![])
        .await
        .map_err(error_text)?;
    apply_snapshot(value, view).await
}

async fn apply_snapshot(value: Value, view: &RwLock<MachineView>) -> ClientResult<()> {
    let snapshot: Snapshot =
        serde_json::from_value(value).map_err(|error| format!("invalid snapshot: {error}"))?;
    validate_id(&snapshot.instance).map_err(|_| "invalid snapshot instance".to_string())?;
    if snapshot.workspaces.len() > 128 {
        return Err("invalid snapshot workspace count".into());
    }

    let mut seen = BTreeSet::new();
    for workspace in &snapshot.workspaces {
        validate_id(&workspace.id)
            .map_err(|_| "invalid snapshot workspace identity".to_string())?;
        if workspace.name.is_empty()
            || workspace.name.len() > 128
            || workspace.name.chars().any(char::is_control)
            || workspace.windows > 4096
            || workspace.panes > 4096
            || !seen.insert(&workspace.id)
        {
            return Err("invalid snapshot workspace".into());
        }
    }
    let mut current = view.write().await;
    if current.instance == snapshot.instance && snapshot.revision < current.revision {
        return Err("invalid snapshot revision regression".into());
    }
    current.instance = snapshot.instance;
    current.revision = snapshot.revision;
    current.workspaces = snapshot.workspaces;
    current.state = "connected".into();
    current.detail.clear();

    Ok(())
}

async fn set_state(view: &RwLock<MachineView>, state: &str, detail: &str) {
    let mut view = view.write().await;
    view.state = state.into();
    view.detail = clean_detail(detail);
}

fn clean_detail(detail: &str) -> String {
    detail
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(512)
        .collect::<String>()
        .trim()
        .to_string()
}

fn needs_attention(detail: &str) -> bool {
    let text = detail.to_ascii_lowercase();
    [
        "permission denied",
        "missing capability",
        "invalid remote",
        "incompatible",
        "control path is too long",
        "host key",
        "host identification",
        "authentication failed",
        "upgrade required",
        "not installed",
        "missing cached",
        "unsafe remote",
        "invalid snapshot",
        "invalid protocol",
        "unsupported protocol",
        "usage:",
    ]
    .iter()
    .any(|pattern| text.contains(pattern))
}

fn reconnect_delay(failures: u32, seed: u64) -> Duration {
    let ceiling = (500u64.saturating_mul(1u64 << failures.min(6))).min(30_000);
    Duration::from_millis(ceiling / 2 + seed % (ceiling / 2 + 1))
}

async fn serve(context: Arc<Context>, mut stream: UnixStream) -> ClientResult<()> {
    let request: protocol::Request =
        timeout(Duration::from_secs(3), protocol::read_async(&mut stream))
            .await
            .map_err(error_text)?
            .map_err(error_text)?;
    let result =
        if request.version != protocol::VERSION || request.id.is_empty() || request.id.len() > 64 {
            Err("invalid helper protocol version or request ID".into())
        } else {
            dispatch(&context, &request.method, request.args).await
        };
    let (result, error) = match result {
        Ok(value) => (value, None),
        Err(error) => (Value::Null, Some(error)),
    };
    let mut response = protocol::Response {
        version: protocol::VERSION,
        id: request.id,
        result,
        error,
    };
    if serde_json::to_vec(&response).map_err(error_text)?.len() > protocol::MAX_MESSAGE {
        response.result = Value::Null;
        response.error =
            Some("response exceeds 64 KiB; filter the workspace browser to one machine".into());
    }
    timeout(
        Duration::from_secs(3),
        protocol::write_async(&mut stream, &response),
    )
    .await
    .map_err(error_text)?
    .map_err(error_text)
}

async fn dispatch(context: &Arc<Context>, method: &str, args: Vec<String>) -> ClientResult<Value> {
    match (method, args.as_slice()) {
        ("client.status", []) => Ok(
            json!({"pid":std::process::id(),"tmux_pid":context.tmux_pid,"server":context.socket,
            "warning":*context.warning.read().await}),
        ),
        ("machine.list", []) => {
            let handles: Vec<_> = context.machines.read().await.values().cloned().collect();
            let mut values = vec![];
            for handle in handles {
                let view = handle.view.read().await;
                values.push(json!({"destination":view.destination,"release_tag":view.release_tag,
                    "state":view.state,"detail":view.detail,"instance":view.instance,"revision":view.revision}));
            }
            Ok(json!(values))
        }
        ("machine.status", [host]) => {
            let handle = handle(context, host).await?;
            let value = handle.view.read().await.clone();
            serde_json::to_value(value).map_err(error_text)
        }
        ("machine.reconnect", [host]) => {
            sync_registry(context).await?;
            let handle = handle(context, host).await?;
            let revision = (*handle.reconnect.borrow()).wrapping_add(1);
            set_state(&handle.view, "connecting", "").await;
            handle.reconnect.send(revision).map_err(error_text)?;
            Ok(Value::Null)
        }
        ("workspace.list-all", hosts) if hosts.len() <= 1 => {
            if hosts.is_empty() || hosts[0] == "local" {
                let _ = request(context, "local", "state.snapshot", vec![]).await;
            }
            let handles: Vec<_> = context.machines.read().await.values().cloned().collect();
            let mut rows = vec![];
            for handle in handles {
                let view = handle.view.read().await;
                if hosts.first().is_some_and(|host| host != &view.destination) {
                    continue;
                }
                for workspace in &view.workspaces {
                    let state = if view.state != "connected" {
                        "stale"
                    } else if workspace.running {
                        "running"
                    } else {
                        "stopped"
                    };
                    rows.push(
                        json!({"machine":view.destination,"id":workspace.id,"name":workspace.name,
                        "state":state,"windows":workspace.windows,"panes":workspace.panes}),
                    );
                }
            }
            Ok(json!(rows))
        }
        ("remote.request", [host, operation, parameters @ ..]) => {
            request(context, host, operation, parameters.to_vec()).await
        }
        ("attachment.open", [host, reference]) => {
            open(context, host, reference).await?;
            Ok(Value::Null)
        }
        ("attachment.forget", [host, id]) => {
            forget(
                context,
                &Attachment {
                    machine: host.clone(),
                    workspace: id.clone(),
                },
            )
            .await?;
            Ok(Value::Null)
        }
        _ => Err("invalid helper operation or arguments".into()),
    }
}

async fn handle(context: &Context, host: &str) -> ClientResult<Arc<MachineHandle>> {
    context
        .machines
        .read()
        .await
        .get(host)
        .cloned()
        .ok_or_else(|| "machine is not registered".into())
}

async fn request(
    context: &Context,
    host: &str,
    method: &str,
    args: Vec<String>,
) -> ClientResult<Value> {
    let handle = handle(context, host).await?;
    if host == "local" {
        let deadline = Instant::now() + Duration::from_secs(6);
        while handle.view.read().await.state == "connecting" && Instant::now() < deadline {
            sleep(Duration::from_millis(50)).await;
        }
    }
    let view = handle.view.read().await;
    if view.state != "connected" {
        return Err(format!("{} is {}: {}", host, view.state, view.detail));
    }
    drop(view);

    let (reply, response) = oneshot::channel();
    handle
        .requests
        .try_send(Pending {
            method: method.into(),
            args,
            deadline: Instant::now() + RPC_TIMEOUT,
            reply,
        })
        .map_err(|_| "machine request queue is full or closed".to_string())?;
    timeout(RPC_TIMEOUT, response)
        .await
        .map_err(|_| {
            if is_mutation(method) {
                "request timed out; outcome unknown: refresh state before retrying".into()
            } else {
                "request timed out".to_string()
            }
        })?
        .map_err(error_text)?
}

fn is_mutation(method: &str) -> bool {
    !matches!(method, "status" | "state.snapshot" | "workspace.list")
}

async fn wait_connected(home: &Path, socket: &Path, host: &str) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline {
        let view = send(home, socket, "machine.status", vec![host.into()]).await?;
        if view["state"] == "connected" {
            return Ok(view);
        }
        if view["state"] == "needs-attention" {
            return Err(text(&view, "detail")?.to_string().into());
        }
        sleep(Duration::from_millis(100)).await;
    }
    Err("machine did not synchronize; inspect tmux machine list".into())
}

async fn open(context: &Context, host: &str, reference: &str) -> ClientResult<()> {
    let resolved = request(context, host, "workspace.resolve", vec![reference.into()]).await?;
    let id = resolved["id"]
        .as_str()
        .ok_or("missing workspace identity")?;
    validate_id(id)?;
    let attachment = Attachment {
        machine: host.into(),
        workspace: id.into(),
    };
    let mut preferences = context.attachments.lock().await;
    let windows = attachment_windows(context).await?;
    if !preferences.saved.attachments.contains(&attachment) {
        if preferences.saved.attachments.len() >= 128 {
            return Err("attachment limit of 128 reached".into());
        }
        preferences.saved.attachments.push(attachment.clone());
        save_preferences(context, &preferences)?;
    }
    if let Some(window) = windows.get(&attachment) {
        tmux(&context.socket, &["select-window", "-t", window]).await?;
    } else {
        preferences.pending.insert(attachment.clone());
        let sessions = tmux(&context.socket, &["list-sessions", "-F", "#{session_id}"]).await?;
        let session = sessions.lines().next().ok_or("local tmux has no session")?;
        let unique = format!("agents-{}", random_id().map_err(error_text)?);
        let target = format!("{session}:={unique}");
        // A trailing semicolon is a tmux command separator even in a native argument vector.
        let label = format!(
            "workspace:{reference}{}",
            if reference.ends_with(';') { " " } else { "" }
        );
        let executable = std::env::current_exe().map_err(error_text)?;
        let executable = executable.to_str().ok_or("executable path must be UTF-8")?;
        let home = format!("TMUX_AGENTS_HOME={}", context.home.display());
        let socket = context.socket.to_str().ok_or("socket path must be UTF-8")?;
        if socket.ends_with(';') || executable.ends_with(';') {
            return Err("attachment socket/executable paths cannot end with a semicolon".into());
        }
        // One tmux command queue tags the window even if the helper dies after creating it.
        tmux(
            &context.socket,
            &[
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{window_id}",
                "-t",
                &format!("{session}:"),
                "-n",
                &unique,
                "env",
                &home,
                executable,
                "attach",
                socket,
                host,
                id,
                ";",
                "set-option",
                "-w",
                "-t",
                &target,
                "@tmux-agents-machine",
                host,
                ";",
                "set-option",
                "-w",
                "-t",
                &target,
                "@tmux-agents-workspace",
                id,
                ";",
                "set-option",
                "-p",
                "-t",
                &target,
                "@tmux-agents-attachment",
                "1",
                ";",
                "select-window",
                "-t",
                &target,
                ";",
                "rename-window",
                "-t",
                &target,
                &label,
            ],
        )
        .await?;
    }
    preferences.pending.remove(&attachment);

    Ok(())
}

fn save_preferences(context: &Context, state: &AttachmentState) -> ClientResult<()> {
    atomic_write(
        &client_path(&context.home, &context.socket, ".json"),
        &serde_json::to_vec_pretty(&state.saved).map_err(error_text)?,
    )
    .map_err(error_text)
}

async fn forget(context: &Context, attachment: &Attachment) -> ClientResult<()> {
    validate_host(&attachment.machine)?;
    validate_id(&attachment.workspace)?;
    let mut state = context.attachments.lock().await;
    state.saved.attachments.retain(|saved| saved != attachment);
    state.pending.remove(attachment);
    save_preferences(context, &state)
}

async fn attachment_windows(context: &Context) -> ClientResult<BTreeMap<Attachment, String>> {
    let output = tmux(&context.socket, &["list-panes", "-a", "-F",
        "#{window_id}\t#{@tmux-agents-machine}\t#{@tmux-agents-workspace}\t#{@tmux-agents-attachment}"]).await?;
    let mut windows = BTreeMap::new();
    for line in output.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 4 || fields[2].is_empty() {
            continue;
        }
        let host = if fields[1].is_empty() {
            "local"
        } else {
            fields[1]
        };
        if fields[3] != "1" && !fields[1].is_empty() {
            continue;
        }
        validate_host(host)?;
        validate_id(fields[2])?;
        windows
            .entry(Attachment {
                machine: host.into(),
                workspace: fields[2].into(),
            })
            .or_insert_with(|| fields[0].to_string());
    }
    Ok(windows)
}

async fn watch_attachments(context: Arc<Context>) {
    let mut tick = interval(Duration::from_secs(2));
    loop {
        tick.tick().await;
        let windows = match attachment_windows(&context).await {
            Ok(windows) => windows,
            Err(_) => continue,
        };
        let pending: Vec<_> = context
            .attachments
            .lock()
            .await
            .pending
            .iter()
            .cloned()
            .collect();
        for attachment in pending {
            let available = match handle(&context, &attachment.machine).await {
                Ok(handle) => handle.view.read().await.state == "connected",
                Err(_) => false,
            };
            if available
                && let Err(error) = open(&context, &attachment.machine, &attachment.workspace).await
            {
                eprintln!("attachment recovery: {error}");
            }
        }
        let mut state = context.attachments.lock().await;
        let removed: Vec<_> = state
            .saved
            .attachments
            .iter()
            .filter(|attachment| {
                !state.pending.contains(*attachment) && !windows.contains_key(*attachment)
            })
            .cloned()
            .collect();
        // Re-read after recovery, otherwise newly restored windows would look intentionally closed.
        let current = if removed.is_empty() {
            windows
        } else {
            match attachment_windows(&context).await {
                Ok(windows) => windows,
                Err(_) => continue,
            }
        };
        let before = state.saved.attachments.len();
        let pending = state.pending.clone();
        state
            .saved
            .attachments
            .retain(|attachment| pending.contains(attachment) || current.contains_key(attachment));
        if before != state.saved.attachments.len()
            && let Err(error) = save_preferences(&context, &state)
        {
            eprintln!("{error}");
        }
    }
}

async fn tmux_pid(socket: &Path) -> ClientResult<u32> {
    tmux(socket, &["display-message", "-p", "#{pid}"])
        .await?
        .trim()
        .parse()
        .map_err(error_text)
}

async fn tmux_pid_now(context: &Context) -> Option<u32> {
    tmux_pid(&context.socket).await.ok()
}

async fn tmux(socket: &Path, args: &[&str]) -> ClientResult<String> {
    let mut command = Command::new("tmux");
    command.arg("-S").arg(socket).args(args).kill_on_drop(true);
    let output = timeout(Duration::from_secs(3), command.output())
        .await
        .map_err(error_text)?
        .map_err(error_text)?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    String::from_utf8(output.stdout).map_err(error_text)
}

fn error_text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_is_bounded_and_jittered() {
        for failure in 0..100 {
            let delay = reconnect_delay(failure, u64::MAX);
            assert!(delay <= Duration::from_secs(30));
            assert!(delay >= Duration::from_millis(250));
        }
        assert_ne!(reconnect_delay(1, 1), reconnect_delay(1, 100));
    }

    #[test]
    fn socket_path_is_short_and_stable() {
        let home = Path::new("/tmp/example");
        assert_eq!(
            socket_path(home, Path::new("/tmp/tmux-501/default")),
            socket_path(home, Path::new("/tmp/tmux-501/default"))
        );
        assert_eq!(
            socket_path(home, Path::new("/tmp/default"))
                .file_name()
                .unwrap()
                .len(),
            17
        );
    }

    #[test]
    fn classify_attention_and_mutations() {
        assert!(needs_attention("Host key verification failed"));
        assert!(needs_attention("Permission denied (publickey)"));
        assert!(!needs_attention("connection reset by peer"));
        assert!(is_mutation("workspace.create"));
        assert!(!is_mutation("state.snapshot"));
    }
}
