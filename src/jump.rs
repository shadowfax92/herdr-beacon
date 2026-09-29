//! Stateless navigation over Herdr status and Agents workspace policy. Callers
//! choose a mode and cursor; this module owns filtering, ordering and the fresh
//! target check. The injected host is also the seam used by navigation tests.
use std::collections::BTreeSet;

use anyhow::{bail, Result};
use serde::Deserialize;

use crate::herdr::{Herdr, HerdrClient};
use crate::model::{AgentObservation, AgentStatus};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JumpOutcome {
    Focused(String),
    Empty,
}

/// The four shortcut policies share one host query and selection flow. Herdr
/// owns what is unread; modes only select from its current lifecycle labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationMode {
    Unread,
    Working,
    Recent,
    RecentReverse,
}

impl NavigationMode {
    fn includes(self, status: AgentStatus) -> bool {
        match self {
            Self::Unread => matches!(status, AgentStatus::Done | AgentStatus::Blocked),
            Self::Working => matches!(status, AgentStatus::Working | AgentStatus::Blocked),
            Self::Recent | Self::RecentReverse => {
                matches!(
                    status,
                    AgentStatus::Idle | AgentStatus::Done | AgentStatus::Unknown
                )
            }
        }
    }

    fn empty_message(self) -> &'static str {
        match self {
            Self::Unread => "No other unread or blocked agents",
            Self::Working => "No working or blocked agents",
            Self::Recent | Self::RecentReverse => "No eligible agents",
        }
    }
}

pub fn navigate_from_environment(mode: NavigationMode) -> Result<JumpOutcome> {
    navigate(
        &Herdr::from_environment(),
        mode,
        invocation_pane().as_deref(),
    )
}

fn invocation_pane() -> Option<String> {
    #[derive(Deserialize)]
    struct Context {
        focused_pane_id: Option<String>,
    }
    std::env::var("HERDR_PLUGIN_CONTEXT_JSON")
        .ok()
        .and_then(|json| serde_json::from_str::<Context>(&json).ok())
        .and_then(|context| context.focused_pane_id)
        .filter(|pane| !pane.is_empty())
        .or_else(|| {
            std::env::var("HERDR_PANE_ID")
                .ok()
                .filter(|pane| !pane.is_empty())
        })
}

fn is_current(agent: &AgentObservation, current_pane: Option<&str>) -> bool {
    // The invoking client's cursor wins over server focus, which may belong to
    // another attached client. Standalone commands fall back to server focus.
    current_pane.map_or(agent.focused, |pane| agent.pane_id == pane)
}

/// Read fresh host truth on every press. No hook delivery, local state directory,
/// or prior navigation is required. Policy/host errors propagate without focus.
pub fn navigate(
    herdr: &impl HerdrClient,
    mode: NavigationMode,
    current_pane: Option<&str>,
) -> Result<JumpOutcome> {
    let policy = herdr.workspace_policy()?;
    let agents = herdr.agent_list()?;
    let mut panes = BTreeSet::new();
    let mut terminals = BTreeSet::new();
    if agents.iter().any(|agent| {
        agent.pane_id.is_empty()
            || agent.terminal_id.is_empty()
            || !panes.insert(&agent.pane_id)
            || !terminals.insert(&agent.terminal_id)
    }) {
        bail!("ambiguous canonical Herdr snapshot");
    }
    let mut candidates: Vec<_> = agents
        .into_iter()
        .filter(|agent| {
            policy.eligible(&agent.workspace_id)
                && (mode.includes(agent.status)
                    // A read completion changes to idle. Keep the invoking pane
                    // only as a cursor so Alt-u continues toward older requests
                    // instead of repeatedly starting at the newest blocker.
                    || (mode == NavigationMode::Unread && is_current(agent, current_pane)))
        })
        .collect();
    candidates.sort_by(|left, right| {
        right
            .state_change_seq
            .cmp(&left.state_change_seq)
            .then_with(|| left.pane_id.cmp(&right.pane_id))
    });
    if candidates.is_empty() {
        herdr.notify(mode.empty_message(), None)?;
        return Ok(JumpOutcome::Empty);
    }
    let current = candidates
        .iter()
        .position(|agent| is_current(agent, current_pane));
    // Reverse traverses the same ring, including pane-ID ties, so forward and
    // reverse undo each other when the host snapshot has not changed.
    let index = match (mode, current) {
        (NavigationMode::RecentReverse, Some(0) | None) => candidates.len() - 1,
        (NavigationMode::RecentReverse, Some(index)) => index - 1,
        (_, Some(index)) => (index + 1) % candidates.len(),
        (_, None) => 0,
    };
    let selected = &candidates[index];
    if mode == NavigationMode::Unread && is_current(selected, current_pane) {
        herdr.notify(mode.empty_message(), None)?;
        return Ok(JumpOutcome::Empty);
    }
    focus_selected(herdr, mode, selected)
}

fn focus_selected(
    herdr: &impl HerdrClient,
    mode: NavigationMode,
    selected: &AgentObservation,
) -> Result<JumpOutcome> {
    // Selection and focus are separate host requests. Recheck identity, policy
    // and mode membership so a moved, replaced, read or newly working target is
    // refused. Herdr has no conditional focus operation, so the final gap remains.
    let current = herdr.agent_get(&selected.pane_id)?;
    let policy = herdr.workspace_policy()?;
    let Some(current) = current else {
        return Ok(JumpOutcome::Empty);
    };
    if current.terminal_id != selected.terminal_id
        || current.pane_id != selected.pane_id
        || current.workspace_id != selected.workspace_id
        || !policy.eligible(&current.workspace_id)
        || !mode.includes(current.status)
    {
        return Ok(JumpOutcome::Empty);
    }
    herdr.focus_agent(&current.pane_id)?;
    Ok(JumpOutcome::Focused(current.pane_id))
}
