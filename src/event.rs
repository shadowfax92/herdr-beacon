use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::herdr::{Herdr, HerdrClient};
use crate::model::ObservationSource;
use crate::state::StateStore;

#[derive(Deserialize)]
struct Envelope {
    event: String,
    data: Value,
}

#[derive(Deserialize)]
struct PaneData {
    pane_id: String,
}

#[derive(Deserialize)]
struct AgentDetectedData {
    pane_id: String,
    #[serde(default)]
    released: bool,
}

#[derive(Deserialize)]
struct MovedData {
    previous_pane_id: String,
    pane: MovedPane,
}

#[derive(Deserialize)]
struct MovedPane {
    pane_id: String,
    workspace_id: String,
    terminal_id: String,
}

pub fn handle_event_from_environment() -> Result<()> {
    let event_name =
        std::env::var("HERDR_PLUGIN_EVENT").context("HERDR_PLUGIN_EVENT is not set")?;
    let payload =
        std::env::var("HERDR_PLUGIN_EVENT_JSON").context("HERDR_PLUGIN_EVENT_JSON is not set")?;
    let store = StateStore::from_environment()?;
    handle_event_json(&Herdr::from_environment(), &store, &event_name, &payload)
}

pub fn handle_event_json(
    herdr: &impl HerdrClient,
    store: &StateStore,
    event_name: &str,
    payload: &str,
) -> Result<()> {
    let envelope = serde_json::from_str::<Envelope>(payload).context("invalid Herdr event JSON")?;
    let expected = event_name.replace('.', "_");
    if envelope.event != expected {
        bail!(
            "Herdr event mismatch: hook is {event_name}, payload is {}",
            envelope.event
        );
    }
    let data_type = envelope
        .data
        .get("type")
        .and_then(Value::as_str)
        .context("Herdr event data omitted type")?;
    if data_type != expected {
        bail!("Herdr event data type mismatch for {event_name}");
    }

    match event_name {
        "pane.agent_status_changed" => {
            let data = parse_data::<PaneData>(envelope.data)?;
            observe_live(herdr, store, &data.pane_id, false)
        }
        "pane.focused" => {
            let data = parse_data::<PaneData>(envelope.data)?;
            observe_live(herdr, store, &data.pane_id, true)
        }
        "pane.closed" | "pane.exited" => {
            let data = parse_data::<PaneData>(envelope.data)?;
            remove_if_missing(herdr, store, &data.pane_id)
        }
        "pane.agent_detected" => {
            let data = parse_data::<AgentDetectedData>(envelope.data)?;
            if data.released {
                remove_if_missing(herdr, store, &data.pane_id)
            } else {
                observe_live(herdr, store, &data.pane_id, false)
            }
        }
        "pane.moved" => {
            let data = parse_data::<MovedData>(envelope.data)?;
            let live = herdr.agent_get(&data.pane.pane_id)?;
            store.update(|state| {
                state.move_pane(
                    &data.previous_pane_id,
                    &data.pane.pane_id,
                    &data.pane.workspace_id,
                    &data.pane.terminal_id,
                );
                if let Some(observation) = live {
                    state.observe(observation, ObservationSource::Event);
                }
                Ok(())
            })
        }
        _ => bail!("unsupported Beacon event hook: {event_name}"),
    }
}

fn observe_live(
    herdr: &impl HerdrClient,
    store: &StateStore,
    pane_id: &str,
    force_focused: bool,
) -> Result<()> {
    let observation = herdr.agent_get(pane_id)?;
    store.update(|state| {
        if let Some(mut observation) = observation {
            observation.focused |= force_focused;
            state.observe(observation, ObservationSource::Event);
        } else {
            state.remove_pane(pane_id);
        }
        Ok(())
    })
}

fn remove_if_missing(herdr: &impl HerdrClient, store: &StateStore, pane_id: &str) -> Result<()> {
    if herdr.agent_get(pane_id)?.is_some() {
        return Ok(());
    }
    store.update(|state| {
        state.remove_pane(pane_id);
        Ok(())
    })
}

fn parse_data<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T> {
    serde_json::from_value(value).context("invalid Herdr event data")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use anyhow::{bail, Result};
    use tempfile::tempdir;

    use super::*;
    use crate::herdr::HerdrClient;
    use crate::model::{AgentObservation, AgentStatus};
    use crate::state::StateStore;

    #[derive(Default)]
    struct FakeHerdr {
        agents: BTreeMap<String, AgentObservation>,
    }

    impl HerdrClient for FakeHerdr {
        fn agent_get(&self, pane_id: &str) -> Result<Option<AgentObservation>> {
            Ok(self.agents.get(pane_id).cloned())
        }

        fn agent_list(&self) -> Result<Vec<AgentObservation>> {
            Ok(self.agents.values().cloned().collect())
        }

        fn focus_agent(&self, _pane_id: &str) -> Result<AgentObservation> {
            bail!("not used by event tests")
        }

        fn notify(&self, _title: &str, _body: Option<&str>) -> Result<()> {
            Ok(())
        }

        fn reload_config(&self) -> Result<()> {
            Ok(())
        }
    }

    fn agent(status: AgentStatus, focused: bool, sequence: u64) -> AgentObservation {
        AgentObservation {
            pane_id: "w1:p1".to_string(),
            terminal_id: "terminal-1".to_string(),
            workspace_id: "w1".to_string(),
            status,
            focused,
            state_change_seq: sequence,
        }
    }

    fn store() -> (tempfile::TempDir, StateStore) {
        let temporary = tempdir().unwrap();
        let store = StateStore::new(temporary.path().join("state"));
        (temporary, store)
    }

    #[test]
    fn status_hook_uses_live_state_instead_of_the_payload_status() {
        let (_temporary, store) = store();
        let fake = FakeHerdr {
            agents: BTreeMap::from([("w1:p1".to_string(), agent(AgentStatus::Working, false, 6))]),
        };
        let payload = r#"{"event":"pane_agent_status_changed","data":{"type":"pane_agent_status_changed","pane_id":"w1:p1","workspace_id":"w1","agent_status":"done"}}"#;

        handle_event_json(&fake, &store, "pane.agent_status_changed", payload).unwrap();

        assert!(store.read().unwrap().newest().is_none());
    }

    #[test]
    fn background_done_and_blocked_agents_are_recorded() {
        for status in [AgentStatus::Done, AgentStatus::Blocked] {
            let (_temporary, store) = store();
            let fake = FakeHerdr {
                agents: BTreeMap::from([("w1:p1".to_string(), agent(status, false, 8))]),
            };
            let payload = format!(
                r#"{{"event":"pane_agent_status_changed","data":{{"type":"pane_agent_status_changed","pane_id":"w1:p1","workspace_id":"w1","agent_status":"{}"}}}}"#,
                match status {
                    AgentStatus::Done => "done",
                    AgentStatus::Blocked => "blocked",
                    _ => unreachable!(),
                }
            );

            handle_event_json(&fake, &store, "pane.agent_status_changed", &payload).unwrap();

            assert_eq!(store.read().unwrap().newest().unwrap().status, status);
        }
    }

    #[test]
    fn focus_hook_clears_blocked_even_after_focus_moves_away() {
        let (_temporary, store) = store();
        store
            .update(|state| {
                state.observe(
                    agent(AgentStatus::Blocked, false, 10),
                    crate::model::ObservationSource::Event,
                );
                Ok(())
            })
            .unwrap();
        let fake = FakeHerdr {
            agents: BTreeMap::from([("w1:p1".to_string(), agent(AgentStatus::Blocked, false, 10))]),
        };
        let payload = r#"{"event":"pane_focused","data":{"type":"pane_focused","pane_id":"w1:p1","workspace_id":"w1"}}"#;

        handle_event_json(&fake, &store, "pane.focused", payload).unwrap();

        assert!(store.read().unwrap().newest().is_none());
    }

    #[test]
    fn release_and_missing_closed_panes_are_removed() {
        for (event, payload) in [
            (
                "pane.agent_detected",
                r#"{"event":"pane_agent_detected","data":{"type":"pane_agent_detected","pane_id":"w1:p1","workspace_id":"w1","released":true}}"#,
            ),
            (
                "pane.closed",
                r#"{"event":"pane_closed","data":{"type":"pane_closed","pane_id":"w1:p1","workspace_id":"w1"}}"#,
            ),
        ] {
            let (_temporary, store) = store();
            store
                .update(|state| {
                    state.observe(
                        agent(AgentStatus::Done, false, 4),
                        crate::model::ObservationSource::Event,
                    );
                    Ok(())
                })
                .unwrap();

            handle_event_json(&FakeHerdr::default(), &store, event, payload).unwrap();

            assert!(store.read().unwrap().newest().is_none());
        }
    }

    #[test]
    fn pane_move_transfers_the_existing_identity() {
        let (_temporary, store) = store();
        store
            .update(|state| {
                state.observe(
                    agent(AgentStatus::Done, false, 14),
                    crate::model::ObservationSource::Event,
                );
                Ok(())
            })
            .unwrap();
        let moved = AgentObservation {
            pane_id: "w2:p3".to_string(),
            terminal_id: "terminal-1".to_string(),
            workspace_id: "w2".to_string(),
            status: AgentStatus::Done,
            focused: false,
            state_change_seq: 14,
        };
        let fake = FakeHerdr {
            agents: BTreeMap::from([("w2:p3".to_string(), moved)]),
        };
        let payload = r#"{"event":"pane_moved","data":{"type":"pane_moved","previous_pane_id":"w1:p1","previous_workspace_id":"w1","previous_tab_id":"w1:t1","pane":{"pane_id":"w2:p3","workspace_id":"w2","terminal_id":"terminal-1"}}}"#;

        handle_event_json(&fake, &store, "pane.moved", payload).unwrap();

        assert_eq!(store.read().unwrap().newest().unwrap().pane_id, "w2:p3");
    }

    #[test]
    fn malformed_or_mismatched_events_fail_closed() {
        let (_temporary, store) = store();
        assert!(
            handle_event_json(&FakeHerdr::default(), &store, "pane.focused", "not-json").is_err()
        );
        assert!(handle_event_json(
            &FakeHerdr::default(),
            &store,
            "workspace.focused",
            r#"{"data":{}}"#
        )
        .is_err());
    }
}
