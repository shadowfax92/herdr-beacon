use std::collections::BTreeSet;

use anyhow::Result;

use crate::herdr::{Herdr, HerdrClient};
use crate::model::{AgentStatus, ObservationSource, UnreadEntry};
use crate::state::StateStore;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JumpOutcome {
    Focused(String),
    Empty,
}

pub fn jump_from_environment() -> Result<JumpOutcome> {
    let store = StateStore::from_environment()?;
    jump_unread(&Herdr::from_environment(), &store)
}

pub fn jump_working_from_environment() -> Result<JumpOutcome> {
    jump_working(&Herdr::from_environment())
}

/// Advances through live working agents without changing Beacon's unread queue.
/// Herdr's status sequence supplies a cross-workspace recency order; the focused
/// working pane is the cursor so repeated invocations visit every active turn.
pub fn jump_working(herdr: &impl HerdrClient) -> Result<JumpOutcome> {
    let mut working = herdr
        .agent_list()?
        .into_iter()
        .filter(|agent| agent.status == AgentStatus::Working)
        .collect::<Vec<_>>();
    working.sort_by(|left, right| {
        right
            .state_change_seq
            .cmp(&left.state_change_seq)
            .then_with(|| left.pane_id.cmp(&right.pane_id))
    });

    if working.is_empty() {
        herdr.notify("No working agents", None)?;
        return Ok(JumpOutcome::Empty);
    }

    let next = working
        .iter()
        .position(|agent| agent.focused)
        .map(|index| (index + 1) % working.len())
        .unwrap_or(0);
    let pane_id = working[next].pane_id.clone();
    herdr.focus_agent(&pane_id)?;
    Ok(JumpOutcome::Focused(pane_id))
}

pub fn jump_unread(herdr: &impl HerdrClient, store: &StateStore) -> Result<JumpOutcome> {
    let agents = herdr.agent_list()?;
    let live_panes = agents
        .iter()
        .map(|agent| agent.pane_id.clone())
        .collect::<BTreeSet<_>>();
    let selected = store.update(|state| {
        for observation in agents {
            state.observe(observation, ObservationSource::Reconcile);
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
        Ok(state.newest().cloned())
    })?;

    let Some(selected) = selected else {
        herdr.notify("No unread agents", None)?;
        return Ok(JumpOutcome::Empty);
    };

    let focused = herdr.focus_agent(&selected.pane_id)?;
    mark_focus_seen(store, focused, &selected)?;
    Ok(JumpOutcome::Focused(selected.pane_id))
}

fn mark_focus_seen(
    store: &StateStore,
    mut focused: crate::model::AgentObservation,
    selected: &UnreadEntry,
) -> Result<()> {
    focused.focused = true;
    store.update(|state| {
        state.observe(focused, ObservationSource::Event);
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

        let outcome = jump_unread(&fake, &store).unwrap();

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

        let outcome = jump_working(&fake).unwrap();

        assert_eq!(outcome, JumpOutcome::Focused("w1:p2".to_string()));
        assert_eq!(*fake.focused.lock().unwrap(), ["w1:p2"]);
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

        let outcome = jump_working(&fake).unwrap();

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

        let outcome = jump_working(&fake).unwrap();

        assert_eq!(outcome, JumpOutcome::Focused("w1:p2".to_string()));
    }

    #[test]
    fn working_jump_reports_an_empty_queue_without_focusing() {
        let fake = FakeHerdr::new(vec![
            agent("w1:p1", AgentStatus::Idle, 8),
            agent("w1:p2", AgentStatus::Done, 12),
        ]);

        let outcome = jump_working(&fake).unwrap();

        assert_eq!(outcome, JumpOutcome::Empty);
        assert!(fake.focused.lock().unwrap().is_empty());
        assert_eq!(*fake.notifications.lock().unwrap(), ["No working agents"]);
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

        let outcome = jump_unread(&fake, &store).unwrap();

        assert_eq!(outcome, JumpOutcome::Empty);
        assert!(store.read().unwrap().entries().is_empty());
    }

    #[test]
    fn reconciliation_does_not_invent_blocked_unread_state() {
        let (_temporary, store) = store();
        let fake = FakeHerdr::new(vec![agent("w1:p1", AgentStatus::Blocked, 7)]);

        let outcome = jump_unread(&fake, &store).unwrap();

        assert_eq!(outcome, JumpOutcome::Empty);
        assert!(store.read().unwrap().entries().is_empty());
        assert_eq!(*fake.notifications.lock().unwrap(), ["No unread agents"]);
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

        assert!(jump_unread(&fake, &store).is_err());

        assert_eq!(store.read().unwrap().newest().unwrap().pane_id, "w1:p1");
    }
}
