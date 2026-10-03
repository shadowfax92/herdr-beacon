//! Agents owns label resolution and the sidebar's unread marks. This adapter
//! validates its fresh bulk snapshot for each navigation. Transport failure stops
//! the action; no cached policy or recovery history can override the next
//! successful response.
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use socket2::{Domain, SockAddr, Socket, Type};

use crate::model::AgentObservation;

const DEADLINE: Duration = Duration::from_secs(2);
const MAX_REPLY: usize = 256 * 1024;
// Version 2 replies add the sidebar's unread marks. Older Agents answers
// unsupported_version, which stops navigation like any other policy failure.
const VERSION: u32 = 2;

/// Only a validated, session-bound snapshot may classify an observation. Display
/// visibility is deliberately absent from this interface: it cannot grant access.
#[derive(Clone, Debug)]
pub struct WorkspacePolicy {
    workspaces: BTreeSet<String>,
    excluded: BTreeSet<String>,
    /// Pane ID to the terminal ID observed in the same Agents frame.
    unread: BTreeMap<String, String>,
}
impl WorkspacePolicy {
    pub fn eligible(&self, workspace: &str) -> bool {
        self.workspaces.contains(workspace) && !self.excluded.contains(workspace)
    }

    /// The Agents sidebar still marks this agent's completion unread (✓ •): it
    /// finished a turn and its pane has not been focused since. Herdr may already
    /// report it idle, because Herdr reads every pane in a visible tab, including
    /// peers hidden behind a zoomed pane. A replaced terminal never matches.
    pub fn marks_unread(&self, agent: &AgentObservation) -> bool {
        self.unread.get(&agent.pane_id) == Some(&agent.terminal_id)
    }

    /// Real and fake adapters enter through identical wire validation.
    pub fn from_reply(bytes: &[u8], session: &str) -> Result<Self> {
        #[derive(Deserialize)]
        struct Reply {
            ok: bool,
            version: u32,
            herdr_socket: String,
            excluded_labels: Vec<String>,
            workspace_ids: Vec<String>,
            excluded_workspace_ids: Vec<String>,
            show_excluded: bool,
            unread: Vec<Unread>,
        }
        #[derive(Deserialize)]
        struct Unread {
            pane_id: String,
            terminal_id: String,
        }
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        if value.get("ok").and_then(|v| v.as_bool()) == Some(false) {
            bail!(
                "Agents policy unavailable: {}: {}",
                value["code"],
                value["error"]
            );
        }
        let reply: Reply = serde_json::from_value(value).context("invalid Agents policy reply")?;
        let session = normalize_socket(session)?;
        if !reply.ok
            || reply.version != VERSION
            || normalize_socket(&reply.herdr_socket)? != session
        {
            bail!("Agents policy version/session mismatch");
        }
        let invalid = |s: &str| s.trim().is_empty() || s.chars().any(char::is_control);
        for values in [
            &reply.excluded_labels,
            &reply.workspace_ids,
            &reply.excluded_workspace_ids,
        ] {
            if values.iter().any(|s| invalid(s)) || values.windows(2).any(|v| v[0] >= v[1]) {
                bail!("Agents policy contains invalid or unsorted sets");
            }
        }
        if reply
            .unread
            .iter()
            .any(|entry| invalid(&entry.pane_id) || invalid(&entry.terminal_id))
            || reply
                .unread
                .windows(2)
                .any(|pair| pair[0].pane_id >= pair[1].pane_id)
        {
            bail!("Agents policy contains invalid or unsorted unread completions");
        }
        let workspaces: BTreeSet<_> = reply.workspace_ids.into_iter().collect();
        let excluded: BTreeSet<_> = reply.excluded_workspace_ids.into_iter().collect();
        if !excluded.is_subset(&workspaces) || excluded.len() > 1984 {
            bail!("Agents policy contains unresolved excluded IDs");
        }
        let _ = reply.show_excluded; // Validated type; sidebar visibility is not eligibility.
        Ok(Self {
            workspaces,
            excluded,
            unread: reply
                .unread
                .into_iter()
                .map(|entry| (entry.pane_id, entry.terminal_id))
                .collect(),
        })
    }
}

pub(crate) fn from_environment() -> Result<WorkspacePolicy> {
    let deadline = Instant::now() + DEADLINE;
    let session = normalize_socket(
        &std::env::var("HERDR_SOCKET_PATH")
            .context("HERDR_SOCKET_PATH is required for Agents policy")?,
    )?;
    // Caller plugin directories belong to Beacon, not the resident Agents owner.
    let root = if let Some(root) = std::env::var_os("HERDR_AGENTS_STATE") {
        PathBuf::from(root)
    } else {
        // The contract uses shell :- semantics: an exported empty XDG value
        // falls back to HOME. The explicit Agents override above stays literal.
        let state = match std::env::var_os("XDG_STATE_HOME").filter(|root| !root.is_empty()) {
            Some(root) => PathBuf::from(root),
            None => PathBuf::from(std::env::var_os("HOME").context("HOME is required")?)
                .join(".local/state"),
        };
        state.join("herdr/plugins/shadowfax.agents")
    };
    exchange(&root.join("control.sock"), &session, deadline)
}

fn normalize_socket(socket: &str) -> Result<String> {
    let path = Path::new(socket);
    if !path.is_absolute() || socket.chars().any(char::is_control) {
        bail!("expected absolute host socket path");
    }
    // Match the frozen Agents lexical normalization, including during restart
    // when the host socket may not exist. Symlinks keep their original spelling.
    let mut normalized = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            part => normalized.push(part),
        }
    }
    Ok(normalized
        .to_str()
        .context("socket must be UTF-8")?
        .to_string())
}

fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .context("Agents policy two-second deadline exceeded")
}

fn exchange(path: &Path, session: &str, deadline: Instant) -> Result<WorkspacePolicy> {
    // std's Unix connect has no timeout. socket2 bounds connection setup without
    // detached threads; every later I/O spends the same remaining request budget.
    let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    socket
        .connect_timeout(&SockAddr::unix(path)?, remaining(deadline)?)
        .context("cannot connect to Agents policy")?;
    let fd: std::os::fd::OwnedFd = socket.into();
    let mut stream = UnixStream::from(fd);
    let mut bytes = serde_json::to_vec(
        &serde_json::json!({"cmd":"workspace_policy","version":VERSION,"herdr_socket":session}),
    )?;
    bytes.push(b'\n');
    let mut written = 0;
    while written < bytes.len() {
        stream.set_write_timeout(Some(remaining(deadline)?))?;
        match stream.write(&bytes[written..]) {
            Ok(0) => bail!("Agents policy write closed"),
            Ok(n) => written += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    let mut reply = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        let count = match stream.read(&mut buffer) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            bail!("Agents policy reply missing newline");
        }
        let newline = buffer[..count].iter().position(|b| *b == b'\n');
        let consumed = newline.map_or(count, |i| i + 1);
        if reply.len() + consumed > MAX_REPLY {
            bail!("Agents policy reply exceeds 256 KiB");
        }
        reply.extend_from_slice(&buffer[..consumed]);
        remaining(deadline)?;
        if newline.is_some() {
            let policy = WorkspacePolicy::from_reply(&reply, session)?;
            remaining(deadline)?;
            return Ok(policy);
        }
    }
}
