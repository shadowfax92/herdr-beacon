use std::collections::BTreeMap;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

pub const STATE_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

impl AgentStatus {
    pub fn needs_attention(self) -> bool {
        matches!(self, Self::Blocked | Self::Done)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservationSource {
    Event,
    Reconcile,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentObservation {
    pub pane_id: String,
    pub terminal_id: String,
    pub workspace_id: String,
    pub status: AgentStatus,
    pub focused: bool,
    pub state_change_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnreadEntry {
    pub pane_id: String,
    pub terminal_id: String,
    pub workspace_id: String,
    pub status: AgentStatus,
    pub state_change_seq: u64,
    pub ordinal: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Watermark {
    terminal_id: String,
    state_change_seq: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeaconState {
    version: u32,
    next_ordinal: u64,
    entries: BTreeMap<String, UnreadEntry>,
    watermarks: BTreeMap<String, Watermark>,
}

impl Default for BeaconState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            next_ordinal: 0,
            entries: BTreeMap::new(),
            watermarks: BTreeMap::new(),
        }
    }
}

impl BeaconState {
    pub fn observe(&mut self, observation: AgentObservation, source: ObservationSource) {
        let reconciled = source == ObservationSource::Reconcile;
        if observation.focused || !observation.status.needs_attention() {
            self.clear_observation(&observation, reconciled);
            return;
        }

        if observation.status == AgentStatus::Blocked
            && reconciled
            && !self.entry_matches(&observation)
        {
            self.rebase_watermark(
                &observation.pane_id,
                &observation.terminal_id,
                observation.state_change_seq,
            );
            return;
        }

        self.record_attention(observation, reconciled);
    }

    pub fn newest(&self) -> Option<&UnreadEntry> {
        self.entries
            .values()
            .max_by_key(|entry| (entry.state_change_seq, entry.ordinal, &entry.pane_id))
    }

    pub fn entries(&self) -> &BTreeMap<String, UnreadEntry> {
        &self.entries
    }

    pub fn remove_pane(&mut self, pane_id: &str) {
        if let Some(entry) = self.entries.remove(pane_id) {
            self.advance_watermark(pane_id, &entry.terminal_id, entry.state_change_seq);
        }
    }

    pub fn move_pane(
        &mut self,
        previous_pane_id: &str,
        pane_id: &str,
        workspace_id: &str,
        terminal_id: &str,
    ) {
        if previous_pane_id == pane_id {
            if let Some(entry) = self.entries.get_mut(pane_id) {
                if entry.terminal_id == terminal_id {
                    entry.workspace_id = workspace_id.to_string();
                }
            }
            return;
        }

        if let Some(mut entry) = self.entries.remove(previous_pane_id) {
            if entry.terminal_id == terminal_id {
                entry.pane_id = pane_id.to_string();
                entry.workspace_id = workspace_id.to_string();
                self.insert_moved_entry(entry);
            }
        }

        if let Some(watermark) = self.watermarks.remove(previous_pane_id) {
            if watermark.terminal_id == terminal_id {
                self.merge_watermark(pane_id, watermark);
            }
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != STATE_VERSION {
            bail!(
                "unsupported Beacon state version {}; expected {STATE_VERSION}",
                self.version
            );
        }
        for (pane_id, entry) in &self.entries {
            if pane_id != &entry.pane_id {
                bail!("Beacon state entry key does not match pane id");
            }
            if entry.ordinal > self.next_ordinal {
                bail!("Beacon state ordinal is ahead of its counter");
            }
        }
        Ok(())
    }

    fn entry_matches(&self, observation: &AgentObservation) -> bool {
        self.entries
            .get(&observation.pane_id)
            .is_some_and(|entry| entry.terminal_id == observation.terminal_id)
    }

    fn record_attention(&mut self, observation: AgentObservation, reconciled: bool) {
        if let Some(watermark) = self.watermarks.get(&observation.pane_id) {
            if watermark.terminal_id == observation.terminal_id
                && observation.state_change_seq <= watermark.state_change_seq
                && !reconciled
            {
                return;
            }
        }

        let existing = self.entries.get(&observation.pane_id).cloned();
        if let Some(entry) = &existing {
            if entry.terminal_id == observation.terminal_id {
                if reconciled {
                    let entry = self.entries.get_mut(&observation.pane_id).unwrap();
                    entry.workspace_id = observation.workspace_id;
                    entry.status = observation.status;
                    entry.state_change_seq = observation.state_change_seq;
                    self.watermarks.remove(&entry.pane_id);
                    return;
                }
                if observation.state_change_seq <= entry.state_change_seq {
                    return;
                }
            }
        }

        self.watermarks.remove(&observation.pane_id);
        self.next_ordinal = self.next_ordinal.saturating_add(1);
        let entry = UnreadEntry {
            pane_id: observation.pane_id.clone(),
            terminal_id: observation.terminal_id,
            workspace_id: observation.workspace_id,
            status: observation.status,
            state_change_seq: observation.state_change_seq,
            ordinal: self.next_ordinal,
        };
        self.entries.insert(observation.pane_id, entry);
    }

    fn clear_observation(&mut self, observation: &AgentObservation, reconciled: bool) {
        let should_remove = self.entries.get(&observation.pane_id).is_some_and(|entry| {
            reconciled
                || entry.terminal_id != observation.terminal_id
                || observation.state_change_seq >= entry.state_change_seq
        });
        if should_remove {
            self.entries.remove(&observation.pane_id);
        }
        if reconciled {
            self.rebase_watermark(
                &observation.pane_id,
                &observation.terminal_id,
                observation.state_change_seq,
            );
        } else {
            self.advance_watermark(
                &observation.pane_id,
                &observation.terminal_id,
                observation.state_change_seq,
            );
        }
    }

    fn advance_watermark(&mut self, pane_id: &str, terminal_id: &str, sequence: u64) {
        let watermark = Watermark {
            terminal_id: terminal_id.to_string(),
            state_change_seq: sequence,
        };
        self.merge_watermark(pane_id, watermark);
    }

    fn rebase_watermark(&mut self, pane_id: &str, terminal_id: &str, sequence: u64) {
        self.watermarks.insert(
            pane_id.to_string(),
            Watermark {
                terminal_id: terminal_id.to_string(),
                state_change_seq: sequence,
            },
        );
    }

    fn merge_watermark(&mut self, pane_id: &str, watermark: Watermark) {
        match self.watermarks.get_mut(pane_id) {
            Some(existing) if existing.terminal_id == watermark.terminal_id => {
                existing.state_change_seq =
                    existing.state_change_seq.max(watermark.state_change_seq);
            }
            _ => {
                self.watermarks.insert(pane_id.to_string(), watermark);
            }
        }
    }

    fn insert_moved_entry(&mut self, entry: UnreadEntry) {
        let should_insert = self.entries.get(&entry.pane_id).is_none_or(|existing| {
            (entry.state_change_seq, entry.ordinal) > (existing.state_change_seq, existing.ordinal)
        });
        if should_insert {
            self.entries.insert(entry.pane_id.clone(), entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(
        pane_id: &str,
        terminal_id: &str,
        status: AgentStatus,
        sequence: u64,
    ) -> AgentObservation {
        AgentObservation {
            pane_id: pane_id.to_string(),
            terminal_id: terminal_id.to_string(),
            workspace_id: "w1".to_string(),
            status,
            focused: false,
            state_change_seq: sequence,
        }
    }

    #[test]
    fn newest_attention_uses_herdr_sequence_then_local_order() {
        let mut state = BeaconState::default();
        state.observe(
            observation("w1:p1", "t1", AgentStatus::Done, 8),
            ObservationSource::Event,
        );
        state.observe(
            observation("w1:p2", "t2", AgentStatus::Blocked, 12),
            ObservationSource::Event,
        );

        assert_eq!(state.newest().unwrap().pane_id, "w1:p2");

        state.observe(
            observation("w1:p3", "t3", AgentStatus::Done, 12),
            ObservationSource::Event,
        );

        assert_eq!(state.newest().unwrap().pane_id, "w1:p3");
    }

    #[test]
    fn watermark_rejects_a_late_attention_hook() {
        let mut state = BeaconState::default();
        let done = observation("w1:p1", "t1", AgentStatus::Done, 5);
        state.observe(done.clone(), ObservationSource::Event);

        let mut focused = done.clone();
        focused.focused = true;
        focused.status = AgentStatus::Idle;
        state.observe(focused, ObservationSource::Event);
        state.observe(done, ObservationSource::Event);

        assert!(state.newest().is_none());

        state.observe(
            observation("w1:p1", "t1", AgentStatus::Done, 6),
            ObservationSource::Event,
        );
        assert_eq!(state.newest().unwrap().state_change_seq, 6);
    }

    #[test]
    fn a_new_terminal_can_reuse_a_watermarked_pane_id() {
        let mut state = BeaconState::default();
        let old = observation("w1:p1", "old", AgentStatus::Done, 40);
        state.observe(old.clone(), ObservationSource::Event);
        let mut focused = old;
        focused.focused = true;
        state.observe(focused, ObservationSource::Event);

        state.observe(
            observation("w1:p1", "new", AgentStatus::Done, 1),
            ObservationSource::Event,
        );

        assert_eq!(state.newest().unwrap().terminal_id, "new");
    }

    #[test]
    fn reconciliation_recovers_done_but_not_unseen_blocked_entries() {
        let mut state = BeaconState::default();
        let done = observation("w1:p1", "t1", AgentStatus::Done, 7);
        state.observe(done.clone(), ObservationSource::Event);
        let mut focused = done.clone();
        focused.focused = true;
        state.observe(focused, ObservationSource::Event);

        state.observe(done, ObservationSource::Reconcile);
        state.observe(
            observation("w1:p2", "t2", AgentStatus::Blocked, 8),
            ObservationSource::Reconcile,
        );

        assert_eq!(state.entries().len(), 1);
        assert_eq!(state.newest().unwrap().pane_id, "w1:p1");
    }

    #[test]
    fn stale_clear_does_not_remove_newer_attention() {
        let mut state = BeaconState::default();
        state.observe(
            observation("w1:p1", "t1", AgentStatus::Done, 9),
            ObservationSource::Event,
        );
        state.observe(
            observation("w1:p1", "t1", AgentStatus::Working, 8),
            ObservationSource::Event,
        );

        assert_eq!(state.newest().unwrap().state_change_seq, 9);
    }

    #[test]
    fn pane_move_preserves_attention_order_and_watermark() {
        let mut state = BeaconState::default();
        state.observe(
            observation("w1:p1", "t1", AgentStatus::Done, 9),
            ObservationSource::Event,
        );
        state.move_pane("w1:p1", "w2:p4", "w2", "t1");

        let entry = state.newest().unwrap();
        assert_eq!(entry.pane_id, "w2:p4");
        assert_eq!(entry.workspace_id, "w2");
        assert_eq!(entry.state_change_seq, 9);
    }

    #[test]
    fn reconciliation_rebases_state_after_herdr_sequence_reset() {
        let mut state = BeaconState::default();
        let old = observation("w1:p1", "t1", AgentStatus::Done, 40);
        state.observe(old.clone(), ObservationSource::Event);
        let mut seen = old;
        seen.focused = true;
        state.observe(seen, ObservationSource::Event);

        state.observe(
            observation("w1:p1", "t1", AgentStatus::Blocked, 1),
            ObservationSource::Reconcile,
        );
        state.observe(
            observation("w1:p1", "t1", AgentStatus::Blocked, 2),
            ObservationSource::Event,
        );

        assert_eq!(state.newest().unwrap().state_change_seq, 2);
    }

    #[test]
    fn authoritative_reconciliation_clears_old_high_sequence_entries() {
        let mut state = BeaconState::default();
        state.observe(
            observation("w1:p1", "t1", AgentStatus::Done, 40),
            ObservationSource::Event,
        );

        state.observe(
            observation("w1:p1", "t1", AgentStatus::Working, 1),
            ObservationSource::Reconcile,
        );

        assert!(state.newest().is_none());
    }
}
