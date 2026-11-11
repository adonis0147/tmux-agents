use std::io;
use std::mem::MaybeUninit;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tokio::time::{sleep, timeout};

use crate::{Result, client, machine, transport};

pub async fn run(home: &Path, socket: &Path, host: &str, id: &str) -> Result<()> {
    client::validate_id(id)?;
    if !socket.is_absolute() {
        return Err("local tmux socket must be absolute".into());
    }
    let canonical = std::fs::canonicalize(socket)?;
    let socket = canonical.as_path();
    if host != "local" {
        machine::validate_destination(host)?;
    }
    let terminal = Terminal::capture()?;
    let mut failures = 0u32;
    let mut retry_at = Instant::now();
    let mut previous = String::new();

    loop {
        terminal.waiting()?;
        if terminal.quit_requested()? {
            finish_detach(home, socket, host, id).await;
            return Ok(());
        }

        let status = timeout(
            Duration::from_millis(700),
            client::send(home, socket, "machine.status", vec![host.into()]),
        )
        .await;
        let connected = match status {
            Ok(Ok(view)) => {
                let state = view["state"].as_str().unwrap_or("unknown");
                let detail = view["detail"].as_str().unwrap_or("");
                let message = format!("{host}: {state} {detail}");
                if previous != message && state != "connected" {
                    eprintln!(
                        "\r\n{message}\r\nWaiting for reconnection. Press q to close this attachment.\r"
                    );
                }
                previous = message;
                state == "connected"
            }
            _ => {
                if previous != "helper unavailable" {
                    eprintln!(
                        "\r\nConnection helper unavailable; restarting it. Press q to close.\r"
                    );
                    previous = "helper unavailable".into();
                }
                let _ = client::ensure(home, socket).await;
                false
            }
        };
        if !connected || Instant::now() < retry_at {
            sleep(Duration::from_millis(100)).await;
            continue;
        }

        terminal.restore();
        let command = if host == "local" {
            let mut command = Command::new(std::env::current_exe()?);
            command
                .args(["terminal", id])
                .env("TMUX_AGENTS_HOME", home)
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
            command
        } else {
            let profile = machine::load(home)?
                .machines
                .into_iter()
                .find(|profile| profile.destination == host)
                .ok_or("machine is no longer registered")?;
            transport::terminal_command(home, socket, &profile, id)?
        };
        let mut command = tokio::process::Command::from(command);
        command.kill_on_drop(true);
        let started = Instant::now();
        let result = command.status().await;
        terminal.restore();

        if result.as_ref().is_ok_and(|status| status.success()) {
            finish_detach(home, socket, host, id).await;
            return Ok(());
        }
        if started.elapsed() >= Duration::from_secs(15) {
            failures = 0;
        }
        let seconds = (1u64 << failures.min(4)).min(10);
        failures = failures.saturating_add(1);
        retry_at = Instant::now() + Duration::from_secs(seconds);
        eprintln!("\r\nAttachment disconnected; retrying in {seconds}s. Press q to close.");
        eprintln!("For authentication recovery, run: tmux machine connect {host}\r");
    }
}

async fn finish_detach(home: &Path, socket: &Path, host: &str, id: &str) {
    loop {
        let _ = client::ensure(home, socket).await;
        if client::send(
            home,
            socket,
            "attachment.forget",
            vec![host.into(), id.into()],
        )
        .await
        .is_ok()
        {
            return;
        }

        // Stay detached until the preference is saved; never turn an intentional detach into a retry.
        eprintln!(
            "\r\nDetached. Waiting to save attachment preferences; no terminal will be reattached.\r"
        );
        sleep(Duration::from_secs(2)).await;
    }
}

struct Terminal {
    saved: libc::termios,
}

impl Terminal {
    fn capture() -> Result<Self> {
        let mut saved = MaybeUninit::uninit();
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, saved.as_mut_ptr()) } != 0 {
            return Err("managed attachments require a terminal".into());
        }
        Ok(Self {
            saved: unsafe { saved.assume_init() },
        })
    }

    fn waiting(&self) -> Result<()> {
        let mut settings = self.saved;
        unsafe {
            libc::cfmakeraw(&mut settings);
        }
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &settings) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }

    fn quit_requested(&self) -> Result<bool> {
        let mut descriptor = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut descriptor, 1, 0) };
        if ready < 0 {
            return Err(io::Error::last_os_error().into());
        }
        if ready == 0 {
            return Ok(false);
        }
        if descriptor.revents & (libc::POLLHUP | libc::POLLERR) != 0 {
            return Ok(true);
        }

        let mut bytes = [0u8; 512];
        let size =
            unsafe { libc::read(libc::STDIN_FILENO, bytes.as_mut_ptr().cast(), bytes.len()) };
        if size < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(size == 0
            || bytes[..size as usize]
                .iter()
                .any(|byte| matches!(byte, b'q' | 3 | 4)))
    }

    fn restore(&self) {
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &self.saved);
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.restore();
    }
}
