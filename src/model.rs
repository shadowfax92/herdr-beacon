//! Host-owned lifecycle and identity values used by navigation. Beacon keeps
//! no completion history; every observation comes from a fresh Herdr query.
use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

/// One canonical live agent. Terminal identity protects against a pane being
/// replaced between selection and focus; the host sequence orders activity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentObservation {
    pub pane_id: String,
    pub terminal_id: String,
    pub workspace_id: String,
    pub status: AgentStatus,
    pub focused: bool,
    pub state_change_seq: u64,
}
