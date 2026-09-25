use std::collections::BTreeSet;

use anyhow::Result;
use serde::Deserialize;

use crate::herdr::{Herdr, HerdrClient};
use crate::model::{AgentObservation, AgentStatus, ObservationSource};
use crate::state::StateStore;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JumpOutcome {
    Focused(String),
    Empty,
}

/// Direction within the same newest-first ring, not a second sort order. Keeping
/// ties in one order makes forward then backward return to the same live agent.
#[derive(Clone, Copy)]
enum Direction {
    Forward,
    Backward,
}

pub fn jump_from_environment() -> Result<JumpOutcome> {
    let store = StateStore::from_environment()?;
    let pane = invocation_pane();
    jump_unread(&Herdr::from_environment(), &store, pane.as_deref())
}

pub fn jump_working_from_environment() -> Result<JumpOutcome> {
    let pane = invocation_pane();
    jump_working(&Herdr::from_environment(), pane.as_deref())
}

pub fn jump_recent_from_environment() -> Result<JumpOutcome> {
    let pane = invocation_pane();
    jump_recent(&Herdr::from_environment(), pane.as_deref())
}

pub fn jump_recent_reverse_from_environment() -> Result<JumpOutcome> {
    let pane = invocation_pane();
    jump_recent_reverse(&Herdr::from_environment(), pane.as_deref())
}

/// Only action entrypoints read this context. A hook's identically named pane
/// field identifies the event target, not the user's current navigation cursor.
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

fn is_current(agent: &crate::model::AgentObservation, current_pane: Option<&str>) -> bool {
    // Preserve standalone CLI behavior when there is no invoking pane context.
    current_pane.map_or(agent.focused, |pane| agent.pane_id == pane)
}

/// Advances through working and blocked agents without directly changing the unread queue.
/// Herdr's status sequence supplies a cross-workspace recency order; the current
/// eligible pane is the cursor, so every active or waiting turn is reachable.
pub fn jump_working(herdr: &impl HerdrClient, current_pane: Option<&str>) -> Result<JumpOutcome> {
    let active_turns = herdr
        .agent_list()?
        .into_iter()
        .filter(is_active_turn)
        .collect();
    jump_in_activity_order(
        herdr,
        active_turns,
        current_pane,
        "No working or blocked agents",
        Direction::Forward,
    )
}

/// Working and blocked turns belong to Alt-O. Both recent-activity directions
/// use its complement, leaving unread completions eligible without consulting or
/// mutating the unread ledger. Normal focus hooks still acknowledge viewed work.
fn is_active_turn(agent: &AgentObservation) -> bool {
    matches!(agent.status, AgentStatus::Working | AgentStatus::Blocked)
}

pub fn jump_recent(herdr: &impl HerdrClient, current_pane: Option<&str>) -> Result<JumpOutcome> {
    jump_recent_in_direction(herdr, current_pane, Direction::Forward)
}

/// Walks back toward newer activity in the same filtered ring as `jump_recent`.
/// From a working/blocked pane or non-agent pane, start at the oldest eligible agent.
pub fn jump_recent_reverse(
    herdr: &impl HerdrClient,
    current_pane: Option<&str>,
) -> Result<JumpOutcome> {
    jump_recent_in_direction(herdr, current_pane, Direction::Backward)
}

fn jump_recent_in_direction(
    herdr: &impl HerdrClient,
    current_pane: Option<&str>,
    direction: Direction,
) -> Result<JumpOutcome> {
    // Membership is refreshed for every press: a newly completed turn joins the
    // cycle immediately, and an agent that resumes work or blocks leaves it.
    let agents = herdr
        .agent_list()?
        .into_iter()
        .filter(|agent| !is_active_turn(agent))
        .collect();
    jump_in_activity_order(herdr, agents, current_pane, "No eligible agents", direction)
}

fn jump_in_activity_order(
    herdr: &impl HerdrClient,
    mut agents: Vec<AgentObservation>,
    current_pane: Option<&str>,
    empty_message: &str,
    direction: Direction,
) -> Result<JumpOutcome> {
    sort_by_activity(&mut agents);
    if agents.is_empty() {
        herdr.notify(empty_message, None)?;
        return Ok(JumpOutcome::Empty);
    }
    let pane_id = agents[cycle_index(&agents, current_pane, direction)]
        .pane_id
        .clone();
    herdr.focus_agent(&pane_id)?;
    Ok(JumpOutcome::Focused(pane_id))
}

/// Status sequences are supplied by one Herdr server across all workspaces.
/// Pane IDs break ties deterministically, including older servers with no sequence.
fn sort_by_activity(agents: &mut [AgentObservation]) {
    agents.sort_by(|left, right| {
        right
            .state_change_seq
            .cmp(&left.state_change_seq)
            .then_with(|| left.pane_id.cmp(&right.pane_id))
    });
}

fn cycle_index(
    agents: &[AgentObservation],
    current_pane: Option<&str>,
    direction: Direction,
) -> usize {
    let current = agents
        .iter()
        .position(|agent| is_current(agent, current_pane));
    match (direction, current) {
        (Direction::Forward, Some(index)) => (index + 1) % agents.len(),
        (Direction::Forward, None) => 0,
        (Direction::Backward, Some(0) | None) => agents.len() - 1,
        (Direction::Backward, Some(index)) => index - 1,
    }
}

/// Unread completions are consumed once; blocked requests remain navigable until
/// resolved. Keep these concepts separate so visiting a blocker cannot recreate
/// an already acknowledged completion in the persisted unread ledger.
pub fn jump_unread(
    herdr: &impl HerdrClient,
    store: &StateStore,
    current_pane: Option<&str>,
) -> Result<JumpOutcome> {
    let selected = store.update(|state| {
        // Serialize the API read with hook reads and ledger writes. Otherwise
        // a list fetched before a newer hook could roll its acknowledgement back.
        let agents = herdr.agent_list()?;
        let live_panes = agents
            .iter()
            .map(|agent| agent.pane_id.clone())
            .collect::<BTreeSet<_>>();
        for observation in &agents {
            let current = is_current(observation, current_pane);
            state.observe(observation.clone(), ObservationSource::Reconcile);
            if current {
                state.observe(observation.clone(), ObservationSource::Focus);
            }
        }
        let missing = state
            .entries()
            .keys()
            .filter(|pane_id| !live_panes.contains(*pane_id))
            .cloned()
            .collect::<Vec<_>>();
        for pane_id in missing {
            state.remove_pane(&pane_id);
        }
        // Retain the calling pane as a cursor even after its completion was
        // acknowledged. Removing it would restart at newest on the next press,
        // repeating a newer blocker before visiting the next older request.
        let mut candidates = agents
            .into_iter()
            .filter(|agent| {
                agent.status == AgentStatus::Blocked
                    || state.entries().contains_key(&agent.pane_id)
                    || is_current(agent, current_pane)
            })
            .collect::<Vec<_>>();
        sort_by_activity(&mut candidates);
        if candidates.is_empty() {
            return Ok(None);
        }
        let selected = &candidates[cycle_index(&candidates, current_pane, Direction::Forward)];
        // A single blocker already on screen is not another destination.
        Ok((!is_current(selected, current_pane)).then(|| selected.clone()))
    })?;

    let Some(selected) = selected else {
        herdr.notify("No other unread or blocked agents", None)?;
        return Ok(JumpOutcome::Empty);
    };

    let focused = herdr.focus_agent(&selected.pane_id)?;
    mark_focus_seen(store, focused, &selected)?;
    Ok(JumpOutcome::Focused(selected.pane_id))
}

fn mark_focus_seen(
    store: &StateStore,
    focused: crate::model::AgentObservation,
    selected: &AgentObservation,
) -> Result<()> {
    store.update(|state| {
        state.observe(focused, ObservationSource::Focus);
        if state.entries().get(&selected.pane_id).is_some_and(|entry| {
            entry.terminal_id == selected.terminal_id
                && entry.state_change_seq <= selected.state_change_seq
        }) {
            state.remove_pane(&selected.pane_id);
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use anyhow::{bail, Result};
    use tempfile::tempdir;

    use super::*;
    use crate::herdr::HerdrClient;
    use crate::model::{AgentObservation, AgentStatus, ObservationSource};
    use crate::state::StateStore;

    struct FakeHerdr {
        agents: Vec<AgentObservation>,
        focus_error: bool,
        focused: Mutex<Vec<String>>,
        notifications: Mutex<Vec<String>>,
    }

    impl FakeHerdr {
        fn new(agents: Vec<AgentObservation>) -> Self {
            Self {
                agents,
                focus_error: false,
                focused: Mutex::new(Vec::new()),
                notifications: Mutex::new(Vec::new()),
            }
        }
    }

    impl HerdrClient for FakeHerdr {
        fn agent_get(&self, pane_id: &str) -> Result<Option<AgentObservation>> {
            Ok(self
                .agents
                .iter()
                .find(|agent| agent.pane_id == pane_id)
                .cloned())
        }

        fn agent_list(&self) -> Result<Vec<AgentObservation>> {
            Ok(self.agents.clone())
        }

        fn focus_agent(&self, pane_id: &str) -> Result<AgentObservation> {
            if self.focus_error {
                bail!("focus failed");
            }
            self.focused.lock().unwrap().push(pane_id.to_string());
            let mut agent = self
                .agents
                .iter()
                .find(|agent| agent.pane_id == pane_id)
                .cloned()
                .unwrap();
            agent.focused = true;
            agent.status = AgentStatus::Idle;
            Ok(agent)
        }

        fn notify(&self, title: &str, _body: Option<&str>) -> Result<()> {
            self.notifications.lock().unwrap().push(title.to_string());
            Ok(())
        }

        fn reload_config(&self) -> Result<()> {
            Ok(())
        }
    }

    fn agent(pane: &str, status: AgentStatus, sequence: u64) -> AgentObservation {
        AgentObservation {
            pane_id: pane.to_string(),
            terminal_id: format!("terminal-{pane}"),
            workspace_id: pane.split(':').next().unwrap().to_string(),
            status,
            focused: false,
            state_change_seq: sequence,
        }
    }

    fn store() -> (tempfile::TempDir, StateStore) {
        let temporary = tempdir().unwrap();
        let store = StateStore::new(temporary.path().join("state"));
        (temporary, store)
    }

    #[test]
    fn jump_recovers_done_agents_and_focuses_the_newest() {
        let (_temporary, store) = store();
        let fake = FakeHerdr::new(vec![
            agent("w1:p1", AgentStatus::Done, 4),
            agent("w2:p1", AgentStatus::Done, 9),
        ]);

        let outcome = jump_unread(&fake, &store, None).unwrap();

        assert_eq!(outcome, JumpOutcome::Focused("w2:p1".to_string()));
        assert_eq!(*fake.focused.lock().unwrap(), ["w2:p1"]);
        let state = store.read().unwrap();
        assert_eq!(state.entries().len(), 1);
        assert_eq!(state.newest().unwrap().pane_id, "w1:p1");
    }

    #[test]
    fn working_jump_advances_from_the_focused_agent_in_recency_order() {
        let mut newest = agent("w1:p1", AgentStatus::Working, 12);
        newest.focused = true;
        let fake = FakeHerdr::new(vec![
            agent("w1:p3", AgentStatus::Working, 4),
            newest,
            agent("w1:p2", AgentStatus::Working, 8),
            agent("w1:p4", AgentStatus::Idle, 20),
        ]);

        let outcome = jump_working(&fake, None).unwrap();

        assert_eq!(outcome, JumpOutcome::Focused("w1:p2".to_string()));
        assert_eq!(*fake.focused.lock().unwrap(), ["w1:p2"]);
    }

    #[test]
    fn working_jump_uses_invoking_pane_instead_of_server_focus() {
        let mut server_focused = agent("w1:p1", AgentStatus::Working, 12);
        server_focused.focused = true;
        let fake = FakeHerdr::new(vec![
            server_focused,
            agent("w1:p2", AgentStatus::Working, 8),
            agent("w1:p3", AgentStatus::Working, 4),
        ]);

        assert_eq!(
            jump_working(&fake, Some("w1:p2")).unwrap(),
            JumpOutcome::Focused("w1:p3".to_string()),
        );
        // A supplied cursor outside the working set must not fall back to server focus.
        assert_eq!(
            jump_working(&fake, Some("w1:p4")).unwrap(),
            JumpOutcome::Focused("w1:p1".to_string()),
        );
    }

    #[test]
    fn recent_jump_skips_working_and_blocked_but_keeps_unread_completions() {
        let fake = FakeHerdr::new(vec![
            agent("w1:p1", AgentStatus::Idle, 8),
            agent("w2:p1", AgentStatus::Blocked, 12),
            agent("w3:p1", AgentStatus::Done, 10),
            agent("w4:p1", AgentStatus::Unknown, 6),
            agent("w5:p1", AgentStatus::Working, 14),
        ]);
        let mut current = "outside".to_string();
        for expected in ["w3:p1", "w1:p1", "w4:p1", "w3:p1"] {
            assert_eq!(
                jump_recent(&fake, Some(&current)).unwrap(),
                JumpOutcome::Focused(expected.to_string())
            );
            current = expected.to_string();
        }
    }

    #[test]
    fn reverse_recent_jump_inverts_forward_at_every_position_including_ties_and_wrap() {
        let fake = FakeHerdr::new(vec![
            agent("w3:p1", AgentStatus::Done, 10),
            agent("w1:p1", AgentStatus::Idle, 8),
            agent("w2:p1", AgentStatus::Idle, 10),
            agent("w5:p1", AgentStatus::Done, 14),
            agent("w4:p1", AgentStatus::Unknown, 6),
        ]);
        for current in &fake.agents {
            let JumpOutcome::Focused(next) = jump_recent(&fake, Some(&current.pane_id)).unwrap()
            else {
                panic!("expected a forward destination");
            };
            assert_eq!(
                jump_recent_reverse(&fake, Some(&next)).unwrap(),
                JumpOutcome::Focused(current.pane_id.clone())
            );
        }
        let mut current = "outside".to_string();
        for expected in ["w4:p1", "w1:p1", "w3:p1", "w2:p1", "w5:p1", "w4:p1"] {
            assert_eq!(
                jump_recent_reverse(&fake, Some(&current)).unwrap(),
                JumpOutcome::Focused(expected.to_string())
            );
            current = expected.to_string();
        }
    }

    #[test]
    fn reverse_recent_jump_respects_invoking_pane_and_handles_empty_single_and_errors() {
        let mut server_focused = agent("w1:p1", AgentStatus::Done, 12);
        server_focused.focused = true;
        let fake = FakeHerdr::new(vec![
            server_focused.clone(),
            agent("w2:p1", AgentStatus::Idle, 8),
        ]);
        assert_eq!(
            jump_recent_reverse(&fake, Some("w2:p1")).unwrap(),
            JumpOutcome::Focused("w1:p1".into())
        );
        assert_eq!(
            jump_recent_reverse(&fake, None).unwrap(),
            JumpOutcome::Focused("w2:p1".into())
        );
        let fake = FakeHerdr::new(vec![]);
        assert_eq!(
            jump_recent_reverse(&fake, None).unwrap(),
            JumpOutcome::Empty
        );
        assert_eq!(*fake.notifications.lock().unwrap(), ["No eligible agents"]);
        let mut fake = FakeHerdr::new(vec![server_focused]);
        assert_eq!(
            jump_recent_reverse(&fake, Some("w1:p1")).unwrap(),
            JumpOutcome::Focused("w1:p1".into())
        );
        fake.focus_error = true;
        assert!(jump_recent_reverse(&fake, None).is_err());
    }

    #[test]
    fn recent_jump_uses_invoking_pane_with_stable_ties() {
        let mut server_focused = agent("w1:p1", AgentStatus::Done, 0);
        server_focused.focused = true;
        let fake = FakeHerdr::new(vec![
            agent("w3:p1", AgentStatus::Idle, 0),
            server_focused,
            agent("w2:p1", AgentStatus::Idle, 0),
        ]);
        assert_eq!(
            jump_recent(&fake, Some("w2:p1")).unwrap(),
            JumpOutcome::Focused("w3:p1".to_string())
        );
        assert_eq!(
            jump_recent(&fake, None).unwrap(),
            JumpOutcome::Focused("w2:p1".to_string())
        );
    }

    #[test]
    fn recent_jump_handles_no_agents_and_propagates_focus_failure() {
        let fake = FakeHerdr::new(vec![]);
        assert_eq!(jump_recent(&fake, None).unwrap(), JumpOutcome::Empty);
        assert_eq!(*fake.notifications.lock().unwrap(), ["No eligible agents"]);
        let mut fake = FakeHerdr::new(vec![agent("w1:p1", AgentStatus::Unknown, 8)]);
        assert_eq!(
            jump_recent(&fake, Some("w1:p1")).unwrap(),
            JumpOutcome::Focused("w1:p1".to_string())
        );
        fake.focus_error = true;
        assert!(jump_recent(&fake, None).is_err());
    }

    #[test]
    fn recent_directions_enter_from_excluded_agents_and_refresh_membership() {
        let mut fake = FakeHerdr::new(vec![
            agent("w1:p1", AgentStatus::Working, 14),
            agent("w2:p1", AgentStatus::Blocked, 12),
            agent("w3:p1", AgentStatus::Done, 10),
            agent("w4:p1", AgentStatus::Idle, 8),
        ]);
        assert_eq!(
            jump_recent(&fake, Some("w1:p1")).unwrap(),
            JumpOutcome::Focused("w3:p1".into())
        );
        assert_eq!(
            jump_recent_reverse(&fake, Some("w2:p1")).unwrap(),
            JumpOutcome::Focused("w4:p1".into())
        );
        // Finishing a turn adds it immediately; starting work removes a candidate.
        fake.agents[0].status = AgentStatus::Idle;
        fake.agents[0].state_change_seq = 15;
        fake.agents[2].status = AgentStatus::Working;
        assert_eq!(
            jump_recent(&fake, Some("w3:p1")).unwrap(),
            JumpOutcome::Focused("w1:p1".into())
        );
        assert_eq!(
            jump_recent_reverse(&fake, Some("w4:p1")).unwrap(),
            JumpOutcome::Focused("w1:p1".into())
        );
    }

    #[test]
    fn recent_directions_do_not_focus_when_all_agents_are_working_or_blocked() {
        let fake = FakeHerdr::new(vec![
            agent("w1:p1", AgentStatus::Working, 14),
            agent("w2:p1", AgentStatus::Blocked, 12),
        ]);
        assert_eq!(jump_recent(&fake, None).unwrap(), JumpOutcome::Empty);
        assert_eq!(
            jump_recent_reverse(&fake, None).unwrap(),
            JumpOutcome::Empty
        );
        assert!(fake.focused.lock().unwrap().is_empty());
    }

    #[test]
    fn working_cycle_includes_blockers_and_excludes_finished_or_unknown_agents() {
        let fake = FakeHerdr::new(vec![
            agent("w1:p1", AgentStatus::Working, 14),
            agent("w2:p1", AgentStatus::Blocked, 12),
            agent("w3:p1", AgentStatus::Done, 20),
            agent("w4:p1", AgentStatus::Idle, 18),
            agent("w5:p1", AgentStatus::Unknown, 16),
        ]);
        assert_eq!(
            jump_working(&fake, Some("w3:p1")).unwrap(),
            JumpOutcome::Focused("w1:p1".into())
        );
        assert_eq!(
            jump_working(&fake, Some("w1:p1")).unwrap(),
            JumpOutcome::Focused("w2:p1".into())
        );
        assert_eq!(
            jump_working(&fake, Some("w2:p1")).unwrap(),
            JumpOutcome::Focused("w1:p1".into())
        );
    }

    #[test]
    fn unread_jump_keeps_api_idle_completion_and_does_not_repeat_it() {
        let (_temporary, store) = store();
        store
            .update(|state| {
                state.observe(
                    agent("w1:p1", AgentStatus::Working, 8),
                    ObservationSource::Event,
                );
                Ok(())
            })
            .unwrap();
        let mut idle = agent("w1:p1", AgentStatus::Idle, 9);
        idle.focused = true;
        let fake = FakeHerdr::new(vec![idle]);

        assert_eq!(
            jump_unread(&fake, &store, Some("w1:p2")).unwrap(),
            JumpOutcome::Focused("w1:p1".to_string()),
        );
        assert_eq!(
            jump_unread(&fake, &store, Some("w1:p2")).unwrap(),
            JumpOutcome::Empty,
        );
        assert_eq!(*fake.focused.lock().unwrap(), ["w1:p1"]);
    }

    #[test]
    fn unread_jump_acknowledges_only_the_invoking_pane() {
        let (_temporary, store) = store();
        let mut other_client = agent("w1:p1", AgentStatus::Done, 8);
        other_client.focused = true;
        let fake = FakeHerdr::new(vec![other_client, agent("w1:p2", AgentStatus::Done, 9)]);
        assert_eq!(
            jump_unread(&fake, &store, Some("w1:p2")).unwrap(),
            JumpOutcome::Focused("w1:p1".to_string()),
        );
        assert!(store.read().unwrap().entries().is_empty());
    }

    #[test]
    fn working_jump_starts_with_the_newest_turn_when_focus_is_elsewhere() {
        let mut idle = agent("w1:p3", AgentStatus::Idle, 20);
        idle.focused = true;
        let fake = FakeHerdr::new(vec![
            agent("w1:p1", AgentStatus::Working, 8),
            idle,
            agent("w1:p2", AgentStatus::Working, 12),
        ]);

        let outcome = jump_working(&fake, None).unwrap();

        assert_eq!(outcome, JumpOutcome::Focused("w1:p2".to_string()));
    }

    #[test]
    fn working_jump_wraps_from_the_oldest_turn_to_the_newest() {
        let mut oldest = agent("w1:p1", AgentStatus::Working, 4);
        oldest.focused = true;
        let fake = FakeHerdr::new(vec![
            oldest,
            agent("w1:p2", AgentStatus::Working, 12),
            agent("w1:p3", AgentStatus::Working, 8),
        ]);

        let outcome = jump_working(&fake, None).unwrap();

        assert_eq!(outcome, JumpOutcome::Focused("w1:p2".to_string()));
    }

    #[test]
    fn working_jump_reports_an_empty_queue_without_focusing() {
        let fake = FakeHerdr::new(vec![
            agent("w1:p1", AgentStatus::Idle, 8),
            agent("w1:p2", AgentStatus::Done, 12),
        ]);

        let outcome = jump_working(&fake, None).unwrap();

        assert_eq!(outcome, JumpOutcome::Empty);
        assert!(fake.focused.lock().unwrap().is_empty());
        assert_eq!(
            *fake.notifications.lock().unwrap(),
            ["No working or blocked agents"]
        );
    }

    #[test]
    fn reconciliation_prunes_missing_focused_and_running_entries() {
        let (_temporary, store) = store();
        store
            .update(|state| {
                for observation in [
                    agent("w1:p1", AgentStatus::Done, 2),
                    agent("w1:p2", AgentStatus::Blocked, 3),
                    agent("w1:p3", AgentStatus::Done, 4),
                ] {
                    state.observe(observation, ObservationSource::Event);
                }
                Ok(())
            })
            .unwrap();
        let mut focused = agent("w1:p2", AgentStatus::Blocked, 3);
        focused.focused = true;
        let fake = FakeHerdr::new(vec![agent("w1:p1", AgentStatus::Working, 5), focused]);

        let outcome = jump_unread(&fake, &store, None).unwrap();

        assert_eq!(outcome, JumpOutcome::Empty);
        assert!(store.read().unwrap().entries().is_empty());
    }

    #[test]
    fn unread_jump_includes_already_blocked_agents_without_inventing_unread_state() {
        let (_temporary, store) = store();
        let fake = FakeHerdr::new(vec![agent("w1:p1", AgentStatus::Blocked, 7)]);

        let outcome = jump_unread(&fake, &store, None).unwrap();

        assert_eq!(outcome, JumpOutcome::Focused("w1:p1".to_string()));
        assert!(store.read().unwrap().entries().is_empty());
    }

    #[test]
    fn unread_jump_cycles_seen_blocked_agents_and_consumes_completions() {
        let (_temporary, store) = store();
        let fake = FakeHerdr::new(vec![
            agent("w1:p1", AgentStatus::Blocked, 12),
            agent("w2:p1", AgentStatus::Done, 10),
            agent("w3:p1", AgentStatus::Blocked, 8),
            agent("w4:p1", AgentStatus::Blocked, 4),
            agent("w5:p1", AgentStatus::Idle, 20),
        ]);
        // Having looked at a blocked request does not resolve its need for input.
        store
            .update(|state| {
                for observation in &fake.agents {
                    if observation.status == AgentStatus::Blocked {
                        state.observe(observation.clone(), ObservationSource::Focus);
                    }
                }
                Ok(())
            })
            .unwrap();
        let mut current = "w5:p1".to_string();
        for expected in ["w1:p1", "w2:p1", "w3:p1", "w4:p1", "w1:p1", "w3:p1"] {
            assert_eq!(
                jump_unread(&fake, &store, Some(&current)).unwrap(),
                JumpOutcome::Focused(expected.to_string())
            );
            current = expected.to_string();
        }
        assert!(store.read().unwrap().entries().is_empty());
    }

    #[test]
    fn unread_jump_wraps_from_an_acknowledged_completion_without_repeating_it() {
        let (_temporary, store) = store();
        let mut current = agent("w1:p1", AgentStatus::Idle, 4);
        current.focused = true;
        let fake = FakeHerdr::new(vec![current, agent("w2:p1", AgentStatus::Blocked, 12)]);
        assert_eq!(
            jump_unread(&fake, &store, Some("w1:p1")).unwrap(),
            JumpOutcome::Focused("w2:p1".to_string())
        );
        assert_eq!(
            jump_unread(&fake, &store, Some("w2:p1")).unwrap(),
            JumpOutcome::Empty
        );
    }

    #[test]
    fn unread_jump_drops_a_resolved_blocker() {
        let (_temporary, store) = store();
        let blocked = agent("w1:p1", AgentStatus::Blocked, 8);
        store
            .update(|state| {
                state.observe(blocked, ObservationSource::Focus);
                Ok(())
            })
            .unwrap();
        let fake = FakeHerdr::new(vec![agent("w1:p1", AgentStatus::Working, 9)]);
        assert_eq!(
            jump_unread(&fake, &store, Some("w2:p1")).unwrap(),
            JumpOutcome::Empty
        );
    }

    #[test]
    fn focus_failure_keeps_the_selected_entry() {
        let (_temporary, store) = store();
        let selected = agent("w1:p1", AgentStatus::Done, 11);
        store
            .update(|state| {
                state.observe(selected.clone(), ObservationSource::Event);
                Ok(())
            })
            .unwrap();
        let mut fake = FakeHerdr::new(vec![selected]);
        fake.focus_error = true;

        assert!(jump_unread(&fake, &store, None).is_err());

        assert_eq!(store.read().unwrap().newest().unwrap().pane_id, "w1:p1");
    }
}
