mod launcher;
mod machines;
mod snapshot;
mod tmux;
mod workspace;

use std::env;
use std::ffi::OsStr;
use std::io::{self, IsTerminal};
use std::path::Path;
use std::process::ExitCode;
use tmux::Tmux;

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("tmux-agents: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    // Private hook entry point, not an interactive management command.
    if arguments.len() == 3 && arguments[0] == "--internal-snapshot" {
        return workspace::save_debounced(
            &Tmux::at(arguments[2].clone().into()),
            Path::new(&arguments[1]),
        )
        .map(|()| ExitCode::SUCCESS);
    }
    if arguments.len() == 2 && arguments[0] == "--internal-remote" {
        return launcher::remote(text(&arguments[1])?);
    }
    if arguments.len() == 2 && arguments[0] == "--internal-take-handoff" {
        return launcher::take_remote_request(text(&arguments[1])?).map(|()| ExitCode::SUCCESS);
    }
    if arguments.len() == 5 && arguments[0] == "--internal-chooser" {
        return launcher::chooser(
            arguments[1].clone().into(),
            text(&arguments[2])?,
            text(&arguments[3])?,
            text(&arguments[4])?,
        )
        .map(|()| ExitCode::SUCCESS);
    }
    if arguments.len() == 5 && arguments[0] == "--internal-switch" {
        let index = text(&arguments[4])?
            .parse()
            .map_err(|_| "invalid machine selection")?;
        return launcher::switch(
            arguments[1].clone().into(),
            text(&arguments[2])?,
            text(&arguments[3])?,
            index,
        )
        .map(|()| ExitCode::SUCCESS);
    }
    if arguments.len() == 1 && (arguments[0] == "--help" || arguments[0] == "-h") {
        println!(
            "Usage: tmux-agents\n       tmux-agents machine add [user@]host\n\n\
             Open or reconnect to your last workspace.\n\
             Register remote machines using your existing SSH configuration."
        );
        return Ok(ExitCode::SUCCESS);
    }
    if arguments.len() == 1 && arguments[0] == "--version" {
        println!("tmux-agents {}", env!("CARGO_PKG_VERSION"));
        return Ok(ExitCode::SUCCESS);
    }
    if arguments.len() == 3 && arguments[0] == "machine" && arguments[1] == "add" {
        let destination = arguments[2]
            .to_str()
            .ok_or("SSH destination must be valid UTF-8")?;
        return machines::add(destination).map(|()| ExitCode::SUCCESS);
    }
    if !arguments.is_empty() {
        return Err("unexpected arguments; run tmux-agents --help for usage".into());
    }

    if env::var_os("TMUX").is_some_and(|value| !value.is_empty()) {
        return Err(
            "already inside tmux; detach first, then run tmux-agents from your outer terminal"
                .into(),
        );
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("an interactive terminal is required".into());
    }

    launcher::run().map(|()| ExitCode::SUCCESS)
}

fn text(value: &OsStr) -> Result<&str, String> {
    value
        .to_str()
        .ok_or_else(|| "internal arguments must be valid UTF-8".into())
}
