use crate::tmux::{Tmux, directory_argument, literal_format, login_shell};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Serialize)]
pub struct Snapshot {
    version: u32,
    active_session: String,
    sessions: Vec<Session>,
}

#[derive(Deserialize, Serialize)]
struct Session {
    name: String,
    active_window: u32,
    windows: Vec<Window>,
}

#[derive(Deserialize, Serialize)]
struct Window {
    index: u32,
    name: String,
    layout: String,
    width: u32,
    height: u32,
    zoomed: bool,
    active_pane: usize,
    directories: Vec<PathBuf>,
}

impl Snapshot {
    pub fn capture(tmux: &Tmux) -> Result<Self, String> {
        let selected = tmux.output(&["show-options", "-gv", "@ta_last_session"])?;
        let active_session = tmux.display(&selected, "#{session_name}")?;
        let mut sessions = Vec::new();
        // shortcut: linked windows restore as copies; preserve links when shared-window restoration is needed.
        for id in tmux
            .output(&["list-sessions", "-F", "#{session_id}"])?
            .lines()
        {
            sessions.push(Session::capture(tmux, id)?);
        }
        let snapshot = Self {
            version: 1,
            active_session,
            sessions,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err(format!(
                "unsupported workspace snapshot version {}",
                self.version
            ));
        }
        let mut names = HashSet::new();
        for session in &self.sessions {
            if session.name.is_empty()
                || session.name.contains([':', '.'])
                || !names.insert(&session.name)
            {
                return Err("invalid or duplicate session name in snapshot".into());
            }
            let mut indices = HashSet::new();
            for window in &session.windows {
                if !indices.insert(window.index)
                    || window.directories.is_empty()
                    || window.active_pane >= window.directories.len()
                    || window.directories.iter().any(|path| !path.is_absolute())
                    || window.layout.is_empty()
                    || window.width == 0
                    || window.height == 0
                {
                    return Err("invalid window in workspace snapshot".into());
                }
            }
            if !indices.contains(&session.active_window) {
                return Err("snapshot has no selected window".into());
            }
        }
        if !names.contains(&self.active_session) {
            return Err("snapshot has no selected session".into());
        }
        Ok(())
    }

    pub fn restore(&self, tmux: &Tmux, fallback: &Path) -> Result<String, String> {
        self.validate()?;
        let mut created = Vec::new();
        let result = self.restore_sessions(tmux, fallback, &mut created);
        if result.is_err() {
            // Only roll back sessions created by this attempt, never pre-existing resources.
            for id in created {
                let _ = tmux.output(&["kill-session", "-t", &id]);
            }
        }
        result
    }

    fn restore_sessions(
        &self,
        tmux: &Tmux,
        fallback: &Path,
        created: &mut Vec<String>,
    ) -> Result<String, String> {
        let mut selected = String::new();
        for session in &self.sessions {
            let first = &session.windows[0];
            let directory = restore_directory(&first.directories[0], fallback)?;
            let identifiers = tmux.output(&[
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{session_id} #{window_id}",
                "-s",
                &literal_format(&session.name),
                "-n",
                &literal_format(&first.name),
                "-c",
                &directory,
                "-x",
                &first.width.to_string(),
                "-y",
                &first.height.to_string(),
                &login_shell()?,
                "-l",
            ])?;
            let (session_id, first_window) = identifiers
                .split_once(' ')
                .ok_or("missing restored tmux IDs")?;
            created.push(session_id.to_owned());
            if session.name == self.active_session {
                selected = session_id.to_owned();
            }
            if tmux.display(first_window, "#{window_index}")? != first.index.to_string() {
                tmux.output(&[
                    "move-window",
                    "-s",
                    first_window,
                    "-t",
                    &format!("{session_id}:{}", first.index),
                ])?;
            }
            let mut active_window = String::new();
            for (position, window) in session.windows.iter().enumerate() {
                let id = if position == 0 {
                    first_window.to_owned()
                } else {
                    tmux.output(&[
                        "new-window",
                        "-d",
                        "-P",
                        "-F",
                        "#{window_id}",
                        "-t",
                        &format!("{session_id}:{}", window.index),
                        "-n",
                        &literal_format(&window.name),
                        "-c",
                        &restore_directory(&window.directories[0], fallback)?,
                        &login_shell()?,
                        "-l",
                    ])?
                };
                window.restore(tmux, &id, fallback)?;
                if window.index == session.active_window {
                    active_window = id;
                }
            }
            tmux.output(&["select-window", "-t", &active_window])?;
        }
        Ok(selected)
    }
}

impl Session {
    fn capture(tmux: &Tmux, id: &str) -> Result<Self, String> {
        let name = tmux.display(id, "#{session_name}")?;
        let active_window = tmux
            .display(id, "#{window_index}")?
            .parse()
            .map_err(|_| "invalid active window index")?;
        let mut windows = Vec::new();
        for index in tmux
            .output(&["list-windows", "-t", id, "-F", "#{window_index}"])?
            .lines()
        {
            // A linked window can have a different index in each session.
            windows.push(Window::capture(tmux, &format!("{id}:{index}"))?);
        }
        Ok(Self {
            name,
            active_window,
            windows,
        })
    }
}

impl Window {
    fn capture(tmux: &Tmux, id: &str) -> Result<Self, String> {
        let metadata = tmux.display(
            id,
            "#{window_index} #{window_width} #{window_height} #{window_zoomed_flag}",
        )?;
        let fields: Vec<_> = metadata.split_whitespace().collect();
        if fields.len() != 4 {
            return Err("invalid tmux window metadata".into());
        }
        let mut directories = Vec::new();
        let mut active_pane = None;
        for (index, pane) in tmux
            .output(&["list-panes", "-t", id, "-F", "#{pane_id}"])?
            .lines()
            .enumerate()
        {
            if tmux.display(pane, "#{pane_active}")? == "1" {
                active_pane = Some(index);
            }
            let current = tmux.display(pane, "#{pane_current_path}")?;
            let directory = if current.is_empty() {
                tmux.display(pane, "#{pane_start_path}")?
            } else {
                current
            };
            directories.push(PathBuf::from(directory));
        }
        Ok(Self {
            index: fields[0].parse().map_err(|_| "invalid window index")?,
            name: tmux.display(id, "#{window_name}")?,
            layout: tmux.display(id, "#{window_layout}")?,
            width: fields[1].parse().map_err(|_| "invalid window width")?,
            height: fields[2].parse().map_err(|_| "invalid window height")?,
            zoomed: fields[3] == "1",
            active_pane: active_pane.ok_or("window has no active pane")?,
            directories,
        })
    }

    fn restore(&self, tmux: &Tmux, id: &str, fallback: &Path) -> Result<(), String> {
        tmux.output(&[
            "resize-window",
            "-t",
            id,
            "-x",
            &self.width.to_string(),
            "-y",
            &self.height.to_string(),
        ])?;
        let mut panes = vec![tmux.display(id, "#{pane_id}")?];
        for directory in &self.directories[1..] {
            let directory = restore_directory(directory, fallback)?;
            let last = panes.last().ok_or("missing restored pane")?;
            let split = |direction| {
                tmux.output(&[
                    "split-window",
                    "-d",
                    direction,
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "-t",
                    last,
                    "-c",
                    &directory,
                    &login_shell()?,
                    "-l",
                ])
            };
            panes.push(split("-v").or_else(|_| split("-h"))?);
            tmux.output(&["select-layout", "-t", id, "tiled"])?;
        }
        // tmux maps saved layout leaves to the newly created panes in pane order.
        tmux.output(&["select-layout", "-t", id, &self.layout])?;
        tmux.output(&["select-pane", "-t", &panes[self.active_pane]])?;
        if self.zoomed {
            tmux.output(&["resize-pane", "-Z", "-t", id])?;
        }
        tmux.output(&["set-option", "-wu", "-t", id, "window-size"])?;
        Ok(())
    }
}

fn restore_directory(directory: &Path, fallback: &Path) -> Result<String, String> {
    if directory.is_dir() {
        directory_argument(directory)
    } else {
        eprintln!(
            "tmux-agents: directory {} is unavailable; using {}",
            directory.display(),
            fallback.display()
        );
        directory_argument(fallback)
    }
}
