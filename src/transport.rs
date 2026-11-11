//! Native SSH transport. Connections are single-flight and failed exchanges are never retried.
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::process::Child;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::Result;
use crate::machine::{self, Machine};
use crate::protocol::{self, Request, Response};

const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(5);
const STDERR_LIMIT: usize = 8192;

#[derive(Debug)]
pub struct ServerError(String);

impl std::fmt::Display for ServerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ServerError {}

pub struct Connection {
    reader: Box<dyn AsyncRead + Send + Unpin>,
    writer: Box<dyn AsyncWrite + Send + Unpin>,
    child: Option<Child>,
    stderr: Option<JoinHandle<Vec<u8>>>,
    failed: bool,
}

pub async fn connect(
    home: &Path,
    tmux_socket: &Path,
    machine: Option<&Machine>,
) -> Result<Connection> {
    if let Some(machine) = machine {
        let mut command = ssh_command(home, tmux_socket, machine)?;
        command.arg("-T");
        command
            .arg(&machine.destination)
            .arg(remote_command(machine, None)?);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut command = tokio::process::Command::from(command);
        command.kill_on_drop(true);
        let mut child = command.spawn()?;
        let reader = Box::new(child.stdout.take().ok_or("missing SSH stdout")?);
        let writer = Box::new(child.stdin.take().ok_or("missing SSH stdin")?);
        let mut stderr = child.stderr.take().ok_or("missing SSH stderr")?;
        let stderr = tokio::spawn(async move {
            let mut captured = Vec::new();
            let mut buffer = [0; 1024];
            // Keep draining after the cap so a verbose SSH process cannot deadlock the transport.
            while let Ok(size) = stderr.read(&mut buffer).await {
                if size == 0 {
                    break;
                }
                let remaining = STDERR_LIMIT - captured.len();
                captured.extend_from_slice(&buffer[..size.min(remaining)]);
            }
            captured
        });
        return Ok(Connection {
            reader,
            writer,
            child: Some(child),
            stderr: Some(stderr),
            failed: false,
        });
    }

    let owned_home = home.to_owned();
    tokio::task::spawn_blocking(move || {
        crate::ensure(&owned_home).map_err(|error| error.to_string())
    })
    .await?
    .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    let socket = tokio::net::UnixStream::connect(home.join("control.sock")).await?;
    let (reader, writer) = socket.into_split();
    Ok(Connection {
        reader: Box::new(reader),
        writer: Box::new(writer),
        child: None,
        stderr: None,
        failed: false,
    })
}

impl Connection {
    pub async fn call(&mut self, method: &str, args: Vec<String>) -> Result<serde_json::Value> {
        if self.failed {
            return Err(
                "connection is closed after a failed exchange; no request was retried".into(),
            );
        }
        let request = Request {
            version: protocol::VERSION,
            id: crate::random_id()?,
            method: method.into(),
            args,
        };
        let exchange = timeout(EXCHANGE_TIMEOUT, async {
            protocol::write_async(&mut self.writer, &request).await?;
            let response: Response = protocol::read_async(&mut self.reader).await?;
            if response.version != protocol::VERSION || response.id != request.id {
                return Err("invalid protocol response version or request ID".into());
            }
            Ok::<Response, Box<dyn std::error::Error>>(response)
        })
        .await;
        let response = match exchange {
            Ok(Ok(response)) => response,
            failure => {
                self.failed = true;
                let reason = match failure {
                    Ok(Err(error)) => error.to_string(),
                    Err(_) => "control request timed out".into(),
                    _ => unreachable!(),
                };
                let mutation = !matches!(method, "status" | "state.snapshot" | "workspace.list");
                return Err(if mutation {
                    format!("{reason}; mutation outcome unknown: refresh before retrying; request was not retried")
                } else {
                    reason
                }.into());
            }
        };
        if let Some(error) = response.error {
            return Err(Box::new(ServerError(error)));
        }
        Ok(response.result)
    }

    pub async fn close(&mut self) -> String {
        self.failed = true;
        self.reader = Box::new(tokio::io::empty());
        self.writer = Box::new(tokio::io::sink());
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        if let Some(mut task) = self.stderr.take() {
            match timeout(EXCHANGE_TIMEOUT, &mut task).await {
                Ok(Ok(bytes)) => return String::from_utf8_lossy(&bytes).trim().to_owned(),
                _ => task.abort(),
            }
        }
        String::new()
    }
}

pub fn terminal_command(
    home: &Path,
    tmux_socket: &Path,
    machine: &Machine,
    id: &str,
) -> Result<Command> {
    validate_id(id)?;
    let mut command = ssh_command(home, tmux_socket, machine)?;
    command
        .arg("-tt")
        .arg(&machine.destination)
        .arg(remote_command(machine, Some(id))?)
        .env_remove("TMUX")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    Ok(command)
}

fn ssh_command(home: &Path, socket: &Path, machine: &Machine) -> Result<Command> {
    machine::validate_destination(&machine.destination)?;
    machine::validate_tag(&machine.release_tag)?;
    let mut command = Command::new("ssh");
    if multiplex_enabled() {
        command
            .arg("-o")
            .arg(format!(
                "ControlPath={}",
                control_path(home, socket, machine)?.display()
            ))
            .args(["-o", "ControlMaster=auto", "-o", "ControlPersist=60"]);
    } else {
        // Disable user-configured sharing too: authentication must then use keys/an agent.
        command.args(["-o", "ControlPath=none", "-o", "ControlMaster=no"]);
    }
    command.args(["-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes"]);
    command.args([
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ServerAliveInterval=5",
        "-o",
        "ServerAliveCountMax=2",
    ]);
    Ok(command)
}

fn multiplex_enabled() -> bool {
    std::env::var_os("TMUX_AGENTS_SSH_MULTIPLEX").as_deref() != Some(std::ffi::OsStr::new("0"))
}

fn control_path(home: &Path, socket: &Path, machine: &Machine) -> Result<PathBuf> {
    if !home.is_absolute() || home.as_os_str().len() > 85 {
        return Err("private SSH state directory must be absolute and at most 85 bytes".into());
    }
    let metadata = std::fs::symlink_metadata(home)?;
    use std::os::unix::fs::MetadataExt;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err("SSH state directory must be private, owned by you, and not a symlink".into());
    }
    let mut hash = DefaultHasher::new();
    socket.hash(&mut hash);
    machine.destination.hash(&mut hash);
    let path = home.join(format!("s{:016x}", hash.finish()));
    // OpenSSH adds a temporary suffix when publishing the control socket.
    if path.as_os_str().len() > 85 {
        return Err("private SSH control path is too long; shorten TMUX_AGENTS_HOME or disable multiplexing".into());
    }
    Ok(path)
}

fn remote_command(machine: &Machine, id: Option<&str>) -> Result<String> {
    machine::validate_tag(&machine.release_tag)?;
    if let Some(id) = id {
        validate_id(id)?;
    }
    // Only validated tag/ID tokens enter this constant script. No workspace names or commands do.
    let script = r#"set -euo pipefail
case "$(uname -s):$(uname -m)" in
    Darwin:x86_64) target=x86_64-apple-darwin ;;
    Darwin:arm64|Darwin:aarch64) target=aarch64-apple-darwin ;;
    Linux:x86_64) target=x86_64-unknown-linux-musl ;;
    Linux:aarch64|Linux:arm64) target=aarch64-unknown-linux-musl ;;
    *) echo 'Unsupported remote platform' >&2; exit 1 ;;
esac
safe_path() {
    [[ ! -L "${1}" && -O "${1}" ]] || return 1
    writable=$(find "${1}" -prune \( -perm -002 -o -perm -020 \) -print) || return 1
    [[ -z "${writable}" ]]
}
base="${HOME}"
if ! safe_path "${base}"; then
    echo 'Unsafe remote home directory' >&2; exit 1
fi
for part in .local share tmux-agents bin "${1}" "${target}"; do
    base="${base}/${part}"
    if [[ ! -d "${base}" ]] || ! safe_path "${base}"; then
        echo 'Unsafe or missing cached release; run machine add/upgrade explicitly' >&2; exit 1
    fi
done
binary="${base}/tmux-agents"
if [[ ! -f "${binary}" || ! -x "${binary}" ]] || ! safe_path "${binary}"; then
    echo 'Unsafe or missing cached executable; run machine add/upgrade explicitly' >&2; exit 1
fi
# A pre-bridge release must fail visibly, never trigger an unattended installation.
set +e
if [[ "${#}" -eq 3 ]]; then
    "${binary}" "${2}" "${3}"
else
    "${binary}" "${2}"
fi
status=${?}
if [[ "${status}" -ne 0 ]]; then
    echo 'Remote bridge/terminal failed; upgrade the registered release explicitly if unsupported' >&2
fi
exit "${status}"
"#;
    let quoted = format!("'{}'", script.replace('\'', "'\\''"));
    Ok(format!(
        "bash -c {quoted} tmux-agents {} {}{}",
        machine.release_tag,
        if id.is_some() { "terminal" } else { "bridge" },
        id.map(|id| format!(" {id}")).unwrap_or_default()
    ))
}

fn validate_id(id: &str) -> Result<()> {
    if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("workspace ID must contain exactly 32 hexadecimal characters".into());
    }
    Ok(())
}

pub async fn authenticate(home: &Path, socket: &Path, machine: &Machine) -> Result<()> {
    if !multiplex_enabled() {
        return Err(
            "SSH multiplexing is disabled; configure key/agent authentication instead".into(),
        );
    }
    let mut check = master_command(home, socket, machine)?;
    check
        .args(["-O", "check"])
        .arg(&machine.destination)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if tokio::process::Command::from(check)
        .status()
        .await?
        .success()
    {
        return Ok(());
    }
    let mut command = master_command(home, socket, machine)?;
    command
        .args(["-MNf", "-o", "ControlMaster=yes", "-o", "ControlPersist=60"])
        .arg(&machine.destination)
        .env_remove("TMUX")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if !tokio::process::Command::from(command)
        .status()
        .await?
        .success()
    {
        return Err("interactive SSH authentication failed".into());
    }
    Ok(())
}

fn master_command(home: &Path, socket: &Path, machine: &Machine) -> Result<Command> {
    machine::validate_destination(&machine.destination)?;
    let mut command = Command::new("ssh");
    command.arg("-S").arg(control_path(home, socket, machine)?);
    Ok(command)
}

pub async fn close_master(home: &Path, socket: &Path, machine: &Machine) {
    if !multiplex_enabled() {
        return;
    }
    if let Ok(mut command) = master_command(home, socket, machine) {
        command
            .args(["-O", "exit"])
            .arg(&machine.destination)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = tokio::process::Command::from(command).status().await;
    }
}

pub fn bridge(home: &Path) -> Result<()> {
    crate::ensure(home)?;
    let mut socket = UnixStream::connect(home.join("control.sock"))?;
    // Use unbuffered duplicates: stdio's line buffering can strand binary protocol frames.
    let mut stdin = duplicate_fd(libc::STDIN_FILENO)?;
    let mut stdout = duplicate_fd(libc::STDOUT_FILENO)?;
    let _flags = Nonblocking::new(&[libc::STDIN_FILENO, libc::STDOUT_FILENO, socket.as_raw_fd()])?;
    let mut upstream = Vec::new();
    let mut downstream = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let mut descriptors = [
            libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: read_event(upstream.len()),
                revents: 0,
            },
            libc::pollfd {
                fd: socket.as_raw_fd(),
                events: read_event(downstream.len())
                    | if upstream.is_empty() {
                        0
                    } else {
                        libc::POLLOUT
                    },
                revents: 0,
            },
            libc::pollfd {
                fd: libc::STDOUT_FILENO,
                events: if downstream.is_empty() {
                    0
                } else {
                    libc::POLLOUT
                },
                revents: 0,
            },
        ];
        if unsafe {
            libc::poll(
                descriptors.as_mut_ptr(),
                descriptors.len() as libc::nfds_t,
                -1,
            )
        } < 0
        {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(io::Error::last_os_error().into());
        }
        if descriptors[1].revents & libc::POLLOUT != 0 {
            drain_buffer(&mut socket, &mut upstream)?;
        }
        if descriptors[2].revents & libc::POLLOUT != 0 {
            drain_buffer(&mut stdout, &mut downstream)?;
        }
        for (index, pending) in [(0, &mut upstream), (1, &mut downstream)] {
            if descriptors[index].revents & libc::POLLIN != 0 {
                let read = if index == 0 {
                    stdin.read(&mut buffer)
                } else {
                    socket.read(&mut buffer)
                };
                match read {
                    Ok(0) => return Ok(()),
                    Ok(size) => pending.extend_from_slice(&buffer[..size]),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error.into()),
                }
            } else if descriptors[index].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)
                != 0
            {
                return Ok(());
            }
        }
        if descriptors[2].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            return Ok(());
        }
    }
}

fn duplicate_fd(fd: RawFd) -> io::Result<std::fs::File> {
    use std::os::fd::FromRawFd;
    let duplicate = unsafe { libc::dup(fd) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(duplicate) })
}

fn read_event(size: usize) -> libc::c_short {
    if size < protocol::MAX_MESSAGE {
        libc::POLLIN
    } else {
        0
    }
}

fn drain_buffer(writer: &mut impl Write, buffer: &mut Vec<u8>) -> io::Result<()> {
    match writer.write(buffer) {
        Ok(0) => Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "bridge output closed",
        )),
        Ok(size) => {
            buffer.drain(..size);
            Ok(())
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

struct Nonblocking(Vec<(RawFd, libc::c_int)>);

impl Nonblocking {
    fn new(descriptors: &[RawFd]) -> io::Result<Self> {
        let mut guard = Self(Vec::new());
        for &fd in descriptors {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                return Err(io::Error::last_os_error());
            }
            guard.0.push((fd, flags));
        }
        Ok(guard)
    }
}

impl Drop for Nonblocking {
    fn drop(&mut self) {
        for &(fd, flags) in &self.0 {
            unsafe {
                libc::fcntl(fd, libc::F_SETFL, flags);
            }
        }
    }
}

pub fn terminal(home: &Path, id: &str) -> Result<()> {
    validate_id(id)?;
    crate::ensure(home)?;
    let response = crate::request(home, "workspace.resolve", vec![id.into()])?;
    let socket = response.result["socket"]
        .as_str()
        .ok_or("missing workspace socket")?;
    let session = response.result["session"]
        .as_str()
        .ok_or("missing workspace session")?;
    let error = Command::new("tmux")
        .arg("-S")
        .arg(socket)
        .arg("attach-session")
        .arg("-t")
        .arg(format!("={session}"))
        .env_remove("TMUX")
        .exec();
    Err(error.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correlates_responses_and_reports_uncertain_mutations() {
        use tokio::io::AsyncWriteExt;
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                for scenario in [
                    "success",
                    "server-error",
                    "wrong-id",
                    "wrong-version",
                    "oversize",
                ] {
                    let (client, mut server) = tokio::io::duplex(protocol::MAX_MESSAGE * 2);
                    let (reader, writer) = tokio::io::split(client);
                    let mut connection = Connection {
                        reader: Box::new(reader),
                        writer: Box::new(writer),
                        child: None,
                        stderr: None,
                        failed: false,
                    };
                    let task = tokio::spawn(async move {
                        let size = server.read_u32().await.unwrap() as usize;
                        let mut bytes = vec![0; size];
                        server.read_exact(&mut bytes).await.unwrap();
                        let request: Request = serde_json::from_slice(&bytes).unwrap();
                        if scenario == "oversize" {
                            server
                                .write_u32(protocol::MAX_MESSAGE as u32 + 1)
                                .await
                                .unwrap();
                        } else {
                            let response = Response {
                                version: if scenario == "wrong-version" {
                                    999
                                } else {
                                    protocol::VERSION
                                },
                                id: if scenario == "wrong-id" {
                                    "wrong".into()
                                } else {
                                    request.id
                                },
                                result: serde_json::json!({"ok": true}),
                                error: (scenario == "server-error").then(|| "rejected".into()),
                            };
                            let bytes = serde_json::to_vec(&response).unwrap();
                            server.write_u32(bytes.len() as u32).await.unwrap();
                            server.write_all(&bytes).await.unwrap();
                        }
                    });
                    let result = connection.call("workspace.create", vec![]).await;
                    match scenario {
                        "success" => assert_eq!(result.unwrap()["ok"], true),
                        "server-error" => assert_eq!(result.unwrap_err().to_string(), "rejected"),
                        _ => {
                            assert!(
                                result
                                    .unwrap_err()
                                    .to_string()
                                    .contains("mutation outcome unknown")
                            );
                            let error = connection
                                .call("workspace.create", vec![])
                                .await
                                .unwrap_err();
                            assert!(error.to_string().contains("no request was retried"));
                        }
                    }
                    task.await.unwrap();
                    assert!(connection.close().await.is_empty());
                }
            });
    }

    #[test]
    fn validates_ids_and_remote_tokens() {
        for id in [
            "",
            "abc",
            "0123456789abcdef0123456789abcde;",
            "../../session",
        ] {
            assert!(validate_id(id).is_err());
        }
        let id = "0123456789abcdef0123456789abcdef";
        assert!(validate_id(id).is_ok());
        let machine = Machine {
            destination: "user@alias".into(),
            release_tag: "v1.2-test".into(),
        };
        let command = remote_command(&machine, Some(id)).unwrap();
        assert!(command.starts_with("bash -c '"));
        assert!(command.ends_with(&format!(" v1.2-test terminal {id}")));
        let invalid = Machine {
            destination: "alias".into(),
            release_tag: "v1;touch /tmp/bad".into(),
        };
        assert!(remote_command(&invalid, None).is_err());
    }

    #[test]
    fn bootstrap_uses_only_safe_cached_executables() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let target = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "x86_64") => "x86_64-apple-darwin",
            ("macos", "aarch64") => "aarch64-apple-darwin",
            ("linux", "x86_64") => "x86_64-unknown-linux-musl",
            ("linux", "aarch64") => "aarch64-unknown-linux-musl",
            _ => return,
        };
        let home = Path::new("/tmp").join(format!("ta-bootstrap-{}", crate::random_id().unwrap()));
        let directory = home.join(".local/share/tmux-agents/bin/v1").join(target);
        std::fs::create_dir_all(&directory).unwrap();
        for ancestor in directory
            .ancestors()
            .take_while(|path| path.starts_with(&home))
        {
            std::fs::set_permissions(ancestor, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let binary = directory.join("tmux-agents");
        std::fs::write(&binary, "#!/bin/bash\nprintf '%s\\n' \"${@}\"\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let machine = Machine {
            destination: "alias".into(),
            release_tag: "v1".into(),
        };
        let run = |id| {
            Command::new("bash")
                .arg("-c")
                .arg(remote_command(&machine, id).unwrap())
                .env("HOME", &home)
                .output()
                .unwrap()
        };
        let output = run(None);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"bridge\n");
        let id = "0123456789abcdef0123456789abcdef";
        let output = run(Some(id));
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("terminal\n{id}\n")
        );
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!run(None).status.success());
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let original = directory.join("original");
        std::fs::rename(&binary, &original).unwrap();
        symlink(&original, &binary).unwrap();
        assert!(!run(None).status.success());
        std::fs::remove_file(&binary).unwrap();
        assert!(!run(None).status.success());
        std::fs::write(&binary, "#!/bin/bash\nexit 2\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let output = run(None);
        assert_eq!(output.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("upgrade the registered release explicitly")
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn private_path_and_native_arguments() {
        use std::os::unix::fs::PermissionsExt;
        let home = Path::new("/tmp").join(format!("ta-ssh-{}", crate::random_id().unwrap()));
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let machine = Machine {
            destination: "user@alias".into(),
            release_tag: "v1".into(),
        };
        let socket = Path::new("/tmp/local.sock");
        let path = control_path(&home, socket, &machine).unwrap();
        assert_eq!(path.file_name().unwrap().len(), 17);
        assert_eq!(path.parent().unwrap(), home);
        assert_eq!(path, control_path(&home, socket, &machine).unwrap());
        assert_ne!(
            path,
            control_path(&home, Path::new("/tmp/other.sock"), &machine).unwrap()
        );
        let command =
            terminal_command(&home, socket, &machine, "0123456789abcdef0123456789abcdef").unwrap();
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(command.get_program(), "ssh");
        assert!(args.contains(&"-tt".into()));
        assert!(args.contains(&"BatchMode=yes".into()));
        assert!(args.contains(&"StrictHostKeyChecking=yes".into()));
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == "TMUX" && value.is_none())
        );
        let long_home = home.join("a".repeat(24));
        std::fs::create_dir(&long_home).unwrap();
        std::fs::set_permissions(&long_home, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            control_path(&long_home, socket, &machine)
                .unwrap_err()
                .to_string()
                .contains("too long")
        );
        std::fs::remove_dir(long_home).unwrap();
        std::fs::remove_dir(home).unwrap();
    }
}
