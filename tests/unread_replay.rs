//! Replays out-of-order hooks through the persisted ledger and unread selector.
//! Only Herdr is replaced: focus requests are recorded, never sent to a live TUI.
use std::cell::RefCell;

use anyhow::{bail, Result};
use herdr_beacon::event::handle_event_json;
use herdr_beacon::herdr::HerdrClient;
use herdr_beacon::jump::{jump_unread, JumpOutcome};
use herdr_beacon::model::{AgentObservation, AgentStatus};
use herdr_beacon::state::StateStore;
use serde_json::{json, Value};
use tempfile::{tempdir, TempDir};

#[derive(Default)]
struct FakeHerdr {
    agents: RefCell<Vec<AgentObservation>>,
    selected: RefCell<Vec<String>>,
}

impl HerdrClient for FakeHerdr {
    fn workspace_policy(&self) -> Result<herdr_beacon::eligibility::WorkspacePolicy> {
        let agents = self.agents.borrow().clone();
        let mut ids: std::collections::BTreeSet<_> =
            agents.iter().map(|a| a.workspace_id.clone()).collect();
        ids.extend(["w1", "w2", "w3", "w4", "w5", "w9"].map(str::to_string));
        herdr_beacon::eligibility::WorkspacePolicy::from_reply(
            &serde_json::to_vec(&serde_json::json!({
                "ok":true,"version":1,"herdr_socket":"/test.sock","excluded_labels":[],
                "workspace_ids":ids,"excluded_workspace_ids":[],"show_excluded":false
            }))?,
            "/test.sock",
        )
    }

    fn agent_get(&self, pane_id: &str) -> Result<Option<AgentObservation>> {
        Ok(self
            .agents
            .borrow()
            .iter()
            .find(|agent| agent.pane_id == pane_id)
            .cloned())
    }

    fn agent_list(&self) -> Result<Vec<AgentObservation>> {
        Ok(self.agents.borrow().clone())
    }

    fn focus_agent(&self, pane_id: &str) -> Result<AgentObservation> {
        self.selected.borrow_mut().push(pane_id.to_string());
        Ok(self.agent_get(pane_id)?.unwrap())
    }

    fn notify(&self, _title: &str, _body: Option<&str>) -> Result<()> {
        Ok(())
    }

    fn reload_config(&self) -> Result<()> {
        bail!("replay must not change configuration")
    }
}

/// Each replay owns its disk state; hook order can vary independently of the
/// current live pane address, just as separately scheduled plugin processes do.
struct Replay {
    _temporary: TempDir,
    herdr: FakeHerdr,
    store: StateStore,
}

impl Replay {
    fn new() -> Self {
        let temporary = tempdir().unwrap();
        let store = StateStore::new(temporary.path().join("state"));
        Self {
            _temporary: temporary,
            herdr: FakeHerdr::default(),
            store,
        }
    }

    fn live(&self, pane: &str, status: AgentStatus, sequence: u64) {
        *self.herdr.agents.borrow_mut() = vec![AgentObservation {
            pane_id: pane.to_string(),
            terminal_id: "same-terminal".to_string(),
            workspace_id: pane.split(':').next().unwrap().to_string(),
            status,
            focused: false,
            state_change_seq: sequence,
        }];
    }

    fn event(&self, name: &str, mut data: Value) {
        let tag = name.replace('.', "_");
        data["type"] = json!(tag);
        handle_event_json(
            &self.herdr,
            &self.store,
            name,
            &json!({"event": tag, "data": data}).to_string(),
        )
        .unwrap();
    }

    fn moved(&self) {
        self.event(
            "pane.moved",
            json!({"previous_pane_id": "w1:p1", "pane": {
                "pane_id": "w2:p2", "workspace_id": "w2",
                "terminal_id": "same-terminal"}}),
        );
    }

    fn assert_no_target(&self) {
        assert_eq!(
            jump_unread(&self.herdr, &self.store, Some("w9:p9")).unwrap(),
            JumpOutcome::Empty,
            "an acknowledged completion must not be selected again"
        );
        assert!(self.herdr.selected.borrow().is_empty());
    }

    fn assert_target(&self) {
        assert_eq!(
            jump_unread(&self.herdr, &self.store, Some("w9:p9")).unwrap(),
            JumpOutcome::Focused("w2:p2".to_string())
        );
        assert_eq!(*self.herdr.selected.borrow(), ["w2:p2"]);
    }
}

#[test]
fn tests_that_delayed_move_preserves_destination_acknowledgement() {
    for status in [AgentStatus::Idle, AgentStatus::Done] {
        let replay = Replay::new();
        replay.live("w1:p1", AgentStatus::Done, 10);
        replay.event("pane.agent_status_changed", json!({"pane_id": "w1:p1"}));
        replay.live("w2:p2", status, 10);
        replay.event("pane.focused", json!({"pane_id": "w2:p2"}));

        // The user already viewed the new address before the move hook ran.
        replay.moved();
        replay.assert_no_target();
    }
}

#[test]
fn tests_that_moved_acknowledgement_clears_destination_pending_entry() {
    let replay = Replay::new();
    replay.live("w1:p1", AgentStatus::Done, 10);
    replay.event("pane.focused", json!({"pane_id": "w1:p1"}));
    replay.live("w2:p2", AgentStatus::Done, 10);
    replay.event("pane.agent_status_changed", json!({"pane_id": "w2:p2"}));

    // A status hook at the new address can run before the acknowledgement's
    // identity transfer. Both hook orders must converge to the same read state.
    replay.moved();
    replay.assert_no_target();
}

#[test]
fn tests_that_reconcile_preserves_ambiguous_legacy_pending_entry() {
    let replay = Replay::new();
    replay.live("w2:p2", AgentStatus::Idle, 10);
    let state_dir = replay._temporary.path().join("state");
    std::fs::create_dir(&state_dir).unwrap();
    // Legacy watermarks do not distinguish a view from an idle baseline or a
    // missing old address. Keep pending work when no clearing evidence survives.
    std::fs::write(
        state_dir.join("state.json"),
        json!({
            "version": 1, "next_ordinal": 1,
            "entries": {"w2:p2": {"pane_id": "w2:p2", "workspace_id": "w2",
                "terminal_id": "same-terminal", "status": "done",
                "state_change_seq": 10, "ordinal": 1}},
            "watermarks": {"w2:p2": {"terminal_id": "same-terminal",
                "state_change_seq": 10}}
        })
        .to_string(),
    )
    .unwrap();

    replay.assert_target();
}

#[test]
fn tests_that_destination_idle_baseline_does_not_acknowledge_a_move() {
    let replay = Replay::new();
    replay.live("w1:p1", AgentStatus::Working, 9);
    replay.event("pane.agent_status_changed", json!({"pane_id": "w1:p1"}));
    replay.live("w1:p1", AgentStatus::Idle, 10);
    replay.event("pane.agent_status_changed", json!({"pane_id": "w1:p1"}));
    replay.live("w2:p2", AgentStatus::Idle, 10);
    replay.event("pane.agent_status_changed", json!({"pane_id": "w2:p2"}));

    replay.moved();
    replay.assert_target();
}

#[test]
fn tests_that_missing_old_address_does_not_acknowledge_a_move() {
    let replay = Replay::new();
    replay.live("w1:p1", AgentStatus::Done, 10);
    replay.event("pane.agent_status_changed", json!({"pane_id": "w1:p1"}));
    replay.live("w2:p2", AgentStatus::Done, 10);
    replay.event("pane.agent_status_changed", json!({"pane_id": "w1:p1"}));
    replay.event("pane.agent_status_changed", json!({"pane_id": "w2:p2"}));

    replay.moved();
    replay.assert_target();
}

#[test]
fn tests_that_move_keeps_a_newer_unseen_idle_completion() {
    let replay = Replay::new();
    replay.live("w1:p1", AgentStatus::Done, 10);
    replay.event("pane.focused", json!({"pane_id": "w1:p1"}));
    replay.live("w2:p2", AgentStatus::Working, 11);
    replay.event("pane.agent_status_changed", json!({"pane_id": "w2:p2"}));
    replay.live("w2:p2", AgentStatus::Idle, 12);
    replay.event("pane.agent_status_changed", json!({"pane_id": "w2:p2"}));
    replay.moved();

    replay.assert_target();
}
