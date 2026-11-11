mod snapshot;
mod tmux;
mod workspace;

use std::env;
use std::io::{self, IsTerminal};
use std::path::Path;
use std::process::ExitCode;
use tmux::Tmux;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("tmux-agents: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    // Private hook entry point, not an interactive management command.
    if arguments.len() == 3 && arguments[0] == "--internal-snapshot" {
        return workspace::save_debounced(
            &Tmux::at(arguments[2].clone().into()),
            Path::new(&arguments[1]),
        );
    }
    if arguments.len() == 1 && (arguments[0] == "--help" || arguments[0] == "-h") {
        println!("Usage: tmux-agents\n\nOpen or reconnect to your local tmux workspace.");
        return Ok(());
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

    let tmux = Tmux::named();
    let (state_directory, session) = workspace::prepare(&tmux)?;
    let status = tmux
        .command()
        .args(["attach-session", "-t", &session])
        .status()
        .map_err(|error| format!("cannot attach to workspace: {error}"))?;
    if let Err(error) = workspace::save_after_detach(&tmux, &state_directory) {
        eprintln!("tmux-agents: {error}");
    }
    if status.success() {
        Ok(())
    } else {
        Err(format!("tmux client exited with {status}"))
    }
}
