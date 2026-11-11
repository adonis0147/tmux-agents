use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;
use std::process::{Command, Output};

use serde::{Deserialize, Serialize};

use crate::{Result, atomic_write, random_id};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct State {
    pub version: u32,
    #[serde(default)]
    pub revision: u64,
    pub workspaces: Vec<Workspace>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Workspace {
    pub id: String,
    pub name: String,
    pub directory: String,
    #[serde(default)]
    pub windows: Vec<Window>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Window {
    pub id: String,
    pub name: String,
    pub layout: String,
    pub panes: Vec<Pane>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Pane {
    pub id: String,
    pub directory: String,
    // The numeric tmux ID occurring in the saved layout, not an application identity.
    pub layout_id: u32,
}

pub(crate) fn load(home: &Path) -> Result<State> {
    let bytes = match fs::read(home.join("metadata.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(State {
                version: 2,
                revision: 0,
                workspaces: vec![],
            });
        }
        Err(error) => return Err(error.into()),
    };
    let mut state: State = serde_json::from_slice(&bytes)?;
    if state.version != 1 && state.version != 2 {
        return Err("incompatible metadata version; saved state was not modified".into());
    }
    validate(&state)?;
    if state.version == 1 {
        if state.revision != 0
            || state
                .workspaces
                .iter()
                .any(|workspace| !workspace.windows.is_empty())
        {
            return Err("invalid v1 metadata; saved state was not modified".into());
        }
        // Back up the exact old bytes before any new-format save or tmux operation.
        let backup = home.join("metadata.v1.backup.json");
        match fs::read(&backup) {
            Ok(existing) if existing == bytes => (),
            Ok(_) => {
                return Err("v1 backup differs from metadata; saved state was not modified".into());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => atomic_write(&backup, &bytes)?,
            Err(error) => return Err(error.into()),
        }
        // Only legacy definitions may claim their pre-marker sessions. New-format state requires ownership.
        for workspace in &state.workspaces {
            if is_running(home, &workspace.id)? {
                checked(
                    home,
                    &[
                        "set-option",
                        "-t",
                        &session_name(&workspace.id),
                        "@tmux_agents_workspace_id",
                        &workspace.id,
                    ],
                )?;
            }
        }
        state.version = 2;
    }
    Ok(state)
}

pub(crate) fn validate(state: &State) -> Result<()> {
    if state.workspaces.len() > 128 {
        return Err("saved state exceeds 128 workspaces".into());
    }
    let mut identities = HashSet::new();
    let mut references = HashSet::new();
    for workspace in &state.workspaces {
        validate_name(&workspace.name)?;
        validate_id(&workspace.id, &mut identities)?;
        if !references.insert(&workspace.id) || !references.insert(&workspace.name) {
            return Err("duplicate saved workspace name or ID".into());
        }
        validate_path(&workspace.directory)?;
        if workspace.windows.len() > 128 {
            return Err("saved workspace exceeds 128 windows".into());
        }
        for window in &workspace.windows {
            validate_id(&window.id, &mut identities)?;
            if window.name.len() > 1024
                || window.name.chars().any(char::is_control)
                || window.name.ends_with(';')
            {
                return Err("invalid window name".into());
            }
            if window.panes.is_empty() || window.panes.len() > 512 {
                return Err("invalid saved pane count".into());
            }
            let leaves = layout_leaves(&window.layout)?;
            let mut pane_numbers = HashSet::new();
            for pane in &window.panes {
                validate_id(&pane.id, &mut identities)?;
                validate_path(&pane.directory)?;
                if !pane_numbers.insert(pane.layout_id) {
                    return Err("duplicate layout pane ID".into());
                }
            }
            if leaves.iter().map(|(_, _, id)| *id).collect::<Vec<_>>()
                != window
                    .panes
                    .iter()
                    .map(|pane| pane.layout_id)
                    .collect::<Vec<_>>()
            {
                return Err("layout does not match saved pane identities".into());
            }
        }
    }
    Ok(())
}

fn validate_id(id: &str, identities: &mut HashSet<String>) -> Result<()> {
    if id.len() != 32
        || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !identities.insert(id.into())
    {
        return Err("invalid or duplicate application ID".into());
    }
    Ok(())
}

pub(crate) fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 128
        || name.trim() != name
        || name.chars().any(char::is_control)
    {
        return Err(
            "workspace name must contain 1..128 bytes without controls or surrounding whitespace"
                .into(),
        );
    }
    Ok(())
}

fn validate_path(directory: &str) -> Result<()> {
    // shortcut: tmux CLI splits trailing semicolons; use a quoted command transport to support these paths.
    if !Path::new(directory).is_absolute()
        || directory.chars().any(char::is_control)
        || directory.ends_with(';')
    {
        return Err(
            "pane directory must be absolute, without controls or a trailing semicolon".into(),
        );
    }
    Ok(())
}

pub(crate) fn save(home: &Path, before: &State, after: &mut State) -> Result<()> {
    validate(after)?;
    after.revision = before
        .revision
        .checked_add(1)
        .ok_or("state revision exhausted")?;
    let shrinking = before.workspaces.iter().any(|old| {
        after
            .workspaces
            .iter()
            .find(|new| new.id == old.id)
            .is_some_and(|new| {
                new.windows.len() < old.windows.len()
                    || new.windows.iter().map(|w| w.panes.len()).sum::<usize>()
                        < old.windows.iter().map(|w| w.panes.len()).sum::<usize>()
            })
    });
    if shrinking {
        atomic_write(
            &home.join("metadata.previous.json"),
            &serde_json::to_vec_pretty(before)?,
        )?;
    }
    atomic_write(
        &home.join("metadata.json"),
        &serde_json::to_vec_pretty(after)?,
    )
}

pub(crate) fn reconcile(home: &Path, state: &mut State) -> Result<()> {
    let mut updated = state.clone();
    let mut failure = None;
    for workspace in &mut updated.workspaces {
        let result = ensure(home, workspace).and_then(|()| capture(home, workspace));
        match result {
            Ok(windows) => workspace.windows = windows,
            Err(error) => failure = Some(error.to_string()),
        }
    }
    if updated != *state || !home.join("metadata.json").exists() {
        save(home, state, &mut updated)?;
        *state = updated;
    }
    if let Some(error) = failure {
        return Err(error.into());
    }
    Ok(())
}

pub(crate) fn ensure(home: &Path, workspace: &Workspace) -> Result<()> {
    if is_running(home, &workspace.id)? {
        return check_owner(home, workspace);
    }
    // Validate every cwd before starting anything. Failure leaves the saved snapshot untouched.
    let directories: Vec<&str> = if workspace.windows.is_empty() {
        vec![&workspace.directory]
    } else {
        workspace
            .windows
            .iter()
            .flat_map(|w| w.panes.iter().map(|p| p.directory.as_str()))
            .collect()
    };
    for directory in directories {
        if !Path::new(directory).is_dir() {
            return Err(format!(
                "cannot restore workspace '{}': directory does not exist: {directory}",
                workspace.name
            )
            .into());
        }
    }
    let session = session_name(&workspace.id);
    let directory = workspace
        .windows
        .first()
        .map(|w| w.panes[0].directory.as_str())
        .unwrap_or(&workspace.directory);
    checked(
        home,
        &[
            "new-session",
            "-d",
            "-s",
            &session,
            "-c",
            directory,
            "-x",
            "200",
            "-y",
            "100",
            "/bin/bash",
            ";",
            "set-option",
            "-t",
            &session,
            "@tmux_agents_restoring",
            "1",
            ";",
            "set-option",
            "-t",
            &session,
            "@tmux_agents_workspace_id",
            &workspace.id,
        ],
    )?;

    // A failed partial restore is never captured as a new good snapshot. Remove only our new session.
    let result = restore_windows(home, &session, &workspace.windows).and_then(|()| {
        checked(
            home,
            &["set-option", "-u", "-t", &session, "@tmux_agents_restoring"],
        )?;
        Ok(())
    });
    if result.is_err() {
        let _ = tmux(home, &["kill-session", "-t", &format!("={session}")]);
    }
    result
}

fn restore_windows(home: &Path, session: &str, windows: &[Window]) -> Result<()> {
    for (index, window) in windows.iter().enumerate() {
        let target = if index == 0 {
            checked(
                home,
                &[
                    "list-windows",
                    "-t",
                    &format!("={session}"),
                    "-F",
                    "#{window_id}",
                ],
            )?
        } else {
            checked(
                home,
                &[
                    "new-window",
                    "-d",
                    "-P",
                    "-F",
                    "#{window_id}",
                    "-t",
                    session,
                    "-c",
                    &window.panes[0].directory,
                    "/bin/bash",
                ],
            )?
        };
        let target = target.trim();
        checked(
            home,
            &["set-option", "-w", "-t", target, "automatic-rename", "off"],
        )?;
        checked(home, &["rename-window", "-t", target, &window.name])?;
        checked(
            home,
            &[
                "set-option",
                "-w",
                "-t",
                target,
                "@tmux_agents_window_id",
                &window.id,
            ],
        )?;
        let first = checked(home, &["list-panes", "-t", target, "-F", "#{pane_id}"])?;
        let mut mapping = vec![];
        let mut split_target = first.trim().to_string();
        for (pane_index, pane) in window.panes.iter().enumerate() {
            let runtime = if pane_index == 0 {
                first.trim().to_string()
            } else {
                let runtime = checked(
                    home,
                    &[
                        "split-window",
                        "-d",
                        "-P",
                        "-F",
                        "#{pane_id}",
                        "-t",
                        &split_target,
                        "-c",
                        &pane.directory,
                        "/bin/bash",
                    ],
                )?;
                checked(home, &["select-layout", "-t", target, "tiled"])?;
                runtime.trim().to_string()
            };
            split_target = runtime.clone();
            checked(
                home,
                &[
                    "set-option",
                    "-p",
                    "-t",
                    &runtime,
                    "@tmux_agents_pane_id",
                    &pane.id,
                ],
            )?;
            mapping.push((
                pane.layout_id,
                runtime.trim_start_matches('%').parse::<u32>()?,
            ));
        }
        let layout = remap_layout(&window.layout, &mapping)?;
        checked(home, &["select-layout", "-t", target, &layout])?;
    }
    Ok(())
}

pub(crate) fn capture(home: &Path, workspace: &Workspace) -> Result<Vec<Window>> {
    check_owner(home, workspace)?;
    let target = format!("={}", session_name(&workspace.id));
    let format = "#{window_id}\t#{@tmux_agents_window_id}\t#{window_name}\t#{window_layout}";
    let listing = checked(home, &["list-windows", "-t", &target, "-F", format])?;
    let mut windows = vec![];
    for line in listing.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 4 || !fields[0].starts_with('@') {
            return Err("unsafe or incomplete window capture".into());
        }
        // Also applies to surviving legacy servers, which do not reload our configuration on restart.
        checked(
            home,
            &["set-option", "-w", "-t", fields[0], "remain-on-exit", "on"],
        )?;
        let id = application_id(home, fields[0], "-w", "@tmux_agents_window_id", fields[1])?;
        let panes = checked(
            home,
            &[
                "list-panes",
                "-t",
                fields[0],
                "-F",
                "#{pane_id}\t#{@tmux_agents_pane_id}\t#{pane_current_path}\t#{pane_dead}\t#{pane_start_path}",
            ],
        )?;
        let mut saved_panes = vec![];
        for line in panes.lines() {
            let parts: Vec<_> = line.split('\t').collect();
            if parts.len() != 5 || !parts[0].starts_with('%') {
                return Err("unsafe or incomplete pane capture".into());
            }
            let pane_id = application_id(home, parts[0], "-p", "@tmux_agents_pane_id", parts[1])?;
            // Dead panes have no OS cwd. Preserve their last observed cwd, never their command.
            let directory = if parts[2].is_empty() && parts[3] == "1" {
                workspace
                    .windows
                    .iter()
                    .flat_map(|window| &window.panes)
                    .find(|pane| pane.id == pane_id)
                    .map(|pane| pane.directory.as_str())
                    .unwrap_or(parts[4])
            } else {
                parts[2]
            };
            validate_path(directory)?;
            saved_panes.push(Pane {
                id: pane_id,
                directory: directory.into(),
                layout_id: parts[0][1..].parse()?,
            });
        }
        let layout = normalize_layout(fields[3])?;
        let leaves = layout_leaves(&layout)?;
        // select-layout assigns panes in traversal order, even when its input contains numeric IDs.
        saved_panes.sort_by_key(|pane| leaves.iter().position(|(_, _, id)| *id == pane.layout_id));
        windows.push(Window {
            id,
            name: fields[2].into(),
            layout,
            panes: saved_panes,
        });
    }
    if windows.is_empty() {
        return Err("incomplete window capture".into());
    }
    // Check again without identity fields: assigning application IDs changes those fields ourselves.
    let verify = checked(
        home,
        &[
            "list-windows",
            "-t",
            &target,
            "-F",
            "#{window_id}\t#{window_layout}",
        ],
    )?;
    let expected = listing
        .lines()
        .map(|line| {
            let fields: Vec<_> = line.split('\t').collect();
            format!("{}\t{}\n", fields[0], fields[3])
        })
        .collect::<String>();
    if verify != expected {
        return Err("layout changed during capture; retrying next poll".into());
    }
    let mut candidate = workspace.clone();
    candidate.windows = windows.clone();
    validate(&State {
        version: 2,
        revision: 0,
        workspaces: vec![candidate],
    })?;
    Ok(windows)
}

fn check_owner(home: &Path, workspace: &Workspace) -> Result<()> {
    let target = session_name(&workspace.id);
    let owner = checked(
        home,
        &[
            "show-options",
            "-qv",
            "-t",
            &target,
            "@tmux_agents_workspace_id",
        ],
    )?;
    if owner.trim() != workspace.id {
        return Err("refusing to adopt an unrelated tmux session with a managed name".into());
    }
    let restoring = checked(
        home,
        &[
            "show-options",
            "-qv",
            "-t",
            &target,
            "@tmux_agents_restoring",
        ],
    )?;
    if restoring.trim() == "1" {
        return Err("incomplete restore session survives; saved snapshot was preserved".into());
    }
    Ok(())
}

fn application_id(
    home: &Path,
    target: &str,
    scope: &str,
    option: &str,
    existing: &str,
) -> Result<String> {
    if !existing.is_empty() {
        validate_id(existing, &mut HashSet::new())?;
        return Ok(existing.into());
    }
    let id = random_id()?;
    checked(home, &["set-option", scope, "-t", target, option, &id])?;
    Ok(id)
}

pub(crate) fn is_running(home: &Path, id: &str) -> Result<bool> {
    let session = format!("={}", session_name(id));
    let output = tmux(home, &["has-session", "-t", &session])?;
    if output.status.success() {
        return Ok(true);
    }
    let error = String::from_utf8_lossy(&output.stderr);
    if error.contains("can't find session")
        || error.contains("no server running")
        || error.contains("error connecting") && !home.join("tmux.sock").exists()
    {
        return Ok(false);
    }
    Err(format!("tmux session check failed: {}", error.trim()).into())
}

pub(crate) fn session_name(id: &str) -> String {
    format!("workspace-{id}")
}

fn checked(home: &Path, args: &[&str]) -> Result<String> {
    let output = tmux(home, args)?;
    if !output.status.success() {
        return Err(format!(
            "tmux {} failed: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn tmux(home: &Path, args: &[&str]) -> Result<Output> {
    Ok(Command::new("tmux")
        .env_remove("TMUX")
        .arg("-S")
        .arg(home.join("tmux.sock"))
        .arg("-f")
        .arg(home.join("tmux.conf"))
        .args(args)
        .output()?)
}

// tmux 3.8 emits JSON layouts; 3.2 emits checksummed strings. Store the portable string form.
fn normalize_layout(layout: &str) -> Result<String> {
    if !layout.starts_with('{') {
        layout_leaves(layout)?;
        return Ok(layout.into());
    }
    let value: serde_json::Value = serde_json::from_str(layout)?;
    if value["V"].as_u64() != Some(2) {
        return Err("unsupported tmux layout format".into());
    }
    let body = json_cell(&value["L"], 0)?;
    let normalized = format!("{:04x},{body}", layout_checksum(&body));
    layout_leaves(&normalized)?;
    Ok(normalized)
}

fn json_cell(cell: &serde_json::Value, depth: usize) -> Result<String> {
    if depth > 128 {
        return Err("tmux layout nesting too deep".into());
    }
    let mut geometry = vec![];
    for key in ["w", "h", "x", "y"] {
        let number = cell[key]
            .as_u64()
            .filter(|number| *number <= 65535)
            .ok_or("invalid JSON layout geometry")?;
        geometry.push(number);
    }
    let mut body = format!(
        "{}x{},{},{}",
        geometry[0], geometry[1], geometry[2], geometry[3]
    );
    match cell["t"].as_str() {
        Some("p") => {
            let runtime = cell["I"]
                .as_str()
                .and_then(|id| id.strip_prefix('%'))
                .ok_or("invalid JSON layout pane ID")?;
            let id: u32 = runtime.parse()?;
            body.push_str(&format!(",{id}"));
        }
        Some(kind @ ("h" | "v")) => {
            let children = cell["c"].as_array().ok_or("missing JSON layout children")?;
            let cells = children
                .iter()
                .map(|child| json_cell(child, depth + 1))
                .collect::<Result<Vec<_>>>()?;
            let (open, close) = if kind == "h" { ('{', '}') } else { ('[', ']') };
            body.push_str(&format!("{open}{}{close}", cells.join(",")));
        }
        _ => return Err("invalid JSON layout cell".into()),
    }
    Ok(body)
}

// Parse the complete layout grammar, including checksum and leaf IDs. Never pass unchecked saved text to tmux.
fn layout_leaves(layout: &str) -> Result<Vec<(usize, usize, u32)>> {
    if layout.len() > 65536
        || !layout.is_ascii()
        || layout.len() < 6
        || layout.as_bytes()[4] != b','
    {
        return Err("invalid tmux layout".into());
    }
    let checksum = u16::from_str_radix(&layout[..4], 16)?;
    if checksum != layout_checksum(&layout[5..]) {
        return Err("invalid tmux layout checksum".into());
    }
    let mut position = 5;
    let mut leaves = vec![];
    let geometry = parse_cell(layout.as_bytes(), &mut position, &mut leaves, 0)?;
    if position != layout.len() || leaves.is_empty() || geometry[2] != 0 || geometry[3] != 0 {
        return Err("invalid tmux layout structure".into());
    }
    Ok(leaves)
}

fn parse_cell(
    bytes: &[u8],
    pos: &mut usize,
    leaves: &mut Vec<(usize, usize, u32)>,
    depth: usize,
) -> Result<[u32; 4]> {
    if depth > 128 {
        return Err("tmux layout nesting too deep".into());
    }
    let mut geometry = [0; 4];
    for (index, delimiter) in b"x,,".iter().enumerate() {
        let value = number(bytes, pos)?;
        geometry[index] = value;
        if value > 65535 || index < 2 && value == 0 {
            return Err("invalid layout size".into());
        }
        if bytes.get(*pos) != Some(delimiter) {
            return Err("invalid layout geometry".into());
        }
        *pos += 1;
    }
    geometry[3] = number(bytes, pos)?;
    if geometry[3] > 65535 {
        return Err("invalid layout offset".into());
    }
    match bytes.get(*pos) {
        Some(b',') => {
            *pos += 1;
            let start = *pos;
            let id = number(bytes, pos)?;
            leaves.push((start, *pos, id));
        }
        Some(open @ (b'[' | b'{')) => {
            let horizontal = *open == b'{';
            let close = if horizontal { b'}' } else { b']' };
            *pos += 1;
            let mut children = vec![parse_cell(bytes, pos, leaves, depth + 1)?];
            while bytes.get(*pos) == Some(&b',') {
                *pos += 1;
                children.push(parse_cell(bytes, pos, leaves, depth + 1)?);
            }
            if children.len() < 2 || bytes.get(*pos) != Some(&close) {
                return Err("invalid layout split".into());
            }
            *pos += 1;
            let (axis, cross_axis) = if horizontal { (0, 1) } else { (1, 0) };
            let offset = axis + 2;
            let cross_offset = cross_axis + 2;
            let mut next = geometry[offset];
            for child in children {
                if child[offset] != next
                    || child[cross_axis] != geometry[cross_axis]
                    || child[cross_offset] != geometry[cross_offset]
                {
                    return Err("inconsistent layout child geometry".into());
                }
                next += child[axis] + 1;
            }
            if next != geometry[offset] + geometry[axis] + 1 {
                return Err("layout children do not fill their parent".into());
            }
        }
        _ => return Err("invalid layout cell".into()),
    }
    Ok(geometry)
}

fn number(bytes: &[u8], pos: &mut usize) -> Result<u32> {
    let start = *pos;
    while bytes.get(*pos).is_some_and(u8::is_ascii_digit) {
        *pos += 1;
    }
    if start == *pos {
        return Err("missing layout number".into());
    }
    Ok(std::str::from_utf8(&bytes[start..*pos])?.parse()?)
}

fn layout_checksum(body: &str) -> u16 {
    body.bytes().fold(0u16, |checksum, byte| {
        checksum.rotate_right(1).wrapping_add(byte as u16)
    })
}

fn remap_layout(layout: &str, mapping: &[(u32, u32)]) -> Result<String> {
    let leaves = layout_leaves(layout)?;
    let mut body = String::new();
    let mut previous = 5;
    for (start, end, old_id) in leaves {
        body.push_str(&layout[previous..start]);
        let new_id = mapping
            .iter()
            .find(|(old, _)| *old == old_id)
            .ok_or("missing layout identity")?
            .1;
        body.push_str(&new_id.to_string());
        previous = end;
    }
    body.push_str(&layout[previous..]);
    Ok(format!("{:04x},{body}", layout_checksum(&body)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_tmux_command_separator_paths() {
        assert!(validate_path("/tmp/safe").is_ok());
        assert!(validate_path("/tmp/unsafe;").is_err());
        assert!(validate_path("/tmp/control\n").is_err());
    }

    #[test]
    fn validates_and_remaps_layouts() {
        let body = "80x24,0,0{40x24,0,0,7,39x24,41,0,12}";
        let layout = format!("{:04x},{body}", layout_checksum(body));
        let mapped = remap_layout(&layout, &[(7, 21), (12, 4)]).unwrap();
        assert_eq!(
            layout_leaves(&mapped)
                .unwrap()
                .iter()
                .map(|(_, _, id)| *id)
                .collect::<Vec<_>>(),
            vec![21, 4]
        );
        assert!(layout_leaves("0000,80x24,0,0,0").is_err());
        assert!(layout_leaves("abcd,run-shell malicious").is_err());
        for body in ["80x24,0,0{40x24,0,0,7,40x24,41,0,12}", "0x24,0,0,1"] {
            assert!(layout_leaves(&format!("{:04x},{body}", layout_checksum(body))).is_err());
        }
    }
}
