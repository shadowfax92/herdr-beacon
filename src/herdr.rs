use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::model::{AgentObservation, AgentStatus};

pub trait HerdrClient {
    fn agent_get(&self, pane_id: &str) -> Result<Option<AgentObservation>>;
    fn agent_list(&self) -> Result<Vec<AgentObservation>>;
    fn focus_agent(&self, pane_id: &str) -> Result<AgentObservation>;
    fn notify(&self, title: &str, body: Option<&str>) -> Result<()>;
    fn reload_config(&self) -> Result<()>;
}

#[derive(Clone, Debug)]
pub struct Herdr {
    binary: PathBuf,
}

impl Herdr {
    pub fn from_environment() -> Self {
        Self {
            binary: select_binary(std::env::var_os("HERDR_BIN_PATH")),
        }
    }

    fn invoke(&self, args: &[String]) -> Result<Invocation> {
        let output = Command::new(&self.binary)
            .args(args)
            .output()
            .with_context(|| format!("failed to run {}", self.binary.display()))?;
        let bytes = if output.status.success() || output.stderr.is_empty() {
            &output.stdout
        } else {
            &output.stderr
        };
        let value = serde_json::from_slice(bytes)
            .with_context(|| format!("{} returned invalid JSON", self.binary.display()))?;
        Ok(Invocation {
            success: output.status.success(),
            value,
        })
    }
}

impl HerdrClient for Herdr {
    fn agent_get(&self, pane_id: &str) -> Result<Option<AgentObservation>> {
        let invocation = self.invoke(&strings(&["agent", "get", pane_id]))?;
        if error_code(&invocation.value) == Some("agent_not_found") {
            return Ok(None);
        }
        require_success(&invocation)?;
        let agent = invocation
            .value
            .pointer("/result/agent")
            .context("Herdr agent get response omitted result.agent")?;
        Ok(Some(parse_agent(agent)?))
    }

    fn agent_list(&self) -> Result<Vec<AgentObservation>> {
        let invocation = self.invoke(&strings(&["agent", "list"]))?;
        require_success(&invocation)?;
        invocation
            .value
            .pointer("/result/agents")
            .and_then(Value::as_array)
            .context("Herdr agent list response omitted result.agents")?
            .iter()
            .map(parse_agent)
            .collect()
    }

    fn focus_agent(&self, pane_id: &str) -> Result<AgentObservation> {
        let invocation = self.invoke(&strings(&["agent", "focus", pane_id]))?;
        require_success(&invocation)?;
        let agent = invocation
            .value
            .pointer("/result/agent")
            .context("Herdr agent focus response omitted result.agent")?;
        parse_agent(agent)
    }

    fn notify(&self, title: &str, body: Option<&str>) -> Result<()> {
        let mut args = strings(&["notification", "show", title]);
        if let Some(body) = body {
            args.extend(strings(&["--body", body]));
        }
        args.extend(strings(&["--sound", "none"]));
        let invocation = self.invoke(&args)?;
        require_success(&invocation)?;
        let shown = invocation
            .value
            .pointer("/result/shown")
            .and_then(Value::as_bool)
            .context("Herdr notification response omitted result.shown")?;
        if !shown {
            let reason = invocation
                .value
                .pointer("/result/reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            // Empty queues are successful navigation outcomes. Herdr may
            // intentionally suppress their best-effort toast (e.g. rapid keys).
            if matches!(
                reason,
                "disabled" | "rate_limited" | "no_foreground_client" | "busy"
            ) {
                return Ok(());
            }
            bail!("Herdr did not show the notification: {reason}");
        }
        Ok(())
    }

    fn reload_config(&self) -> Result<()> {
        let invocation = self.invoke(&strings(&["server", "reload-config"]))?;
        require_success(&invocation)
    }
}

#[derive(Debug)]
struct Invocation {
    success: bool,
    value: Value,
}

#[derive(Deserialize)]
struct AgentRecord {
    terminal_id: String,
    agent_status: AgentStatus,
    workspace_id: String,
    pane_id: String,
    focused: bool,
    #[serde(default)]
    state_change_seq: u64,
}

fn parse_agent(value: &Value) -> Result<AgentObservation> {
    let agent = serde_json::from_value::<AgentRecord>(value.clone())
        .context("invalid Herdr agent payload")?;
    Ok(AgentObservation {
        pane_id: agent.pane_id,
        terminal_id: agent.terminal_id,
        workspace_id: agent.workspace_id,
        status: agent.agent_status,
        focused: agent.focused,
        state_change_seq: agent.state_change_seq,
    })
}

fn select_binary(injected: Option<OsString>) -> PathBuf {
    injected
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("herdr"))
}

fn require_success(invocation: &Invocation) -> Result<()> {
    if invocation.success && invocation.value.get("error").is_none() {
        return Ok(());
    }
    let code = error_code(&invocation.value).unwrap_or("command_failed");
    let message = invocation
        .value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("Herdr command failed");
    bail!("{code}: {message}")
}

fn error_code(value: &Value) -> Option<&str> {
    value.pointer("/error/code").and_then(Value::as_str)
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn agent_response_maps_the_fields_beacon_uses() {
        let response = serde_json::json!({
            "result": {
                "type": "agent_get",
                "agent": {
                    "terminal_id": "terminal-7",
                    "agent_status": "done",
                    "workspace_id": "w2",
                    "tab_id": "w2:t1",
                    "pane_id": "w2:p4",
                    "focused": false,
                    "state_change_seq": 19,
                    "revision": 2
                }
            }
        });

        let observation = parse_agent(&response["result"]["agent"]).unwrap();

        assert_eq!(observation.pane_id, "w2:p4");
        assert_eq!(observation.terminal_id, "terminal-7");
        assert_eq!(observation.status, crate::model::AgentStatus::Done);
        assert_eq!(observation.state_change_seq, 19);
    }

    #[test]
    fn stale_injected_binary_falls_back_to_path_lookup() {
        let temporary = tempdir().unwrap();
        let stale = temporary.path().join("deleted-herdr");

        assert_eq!(
            select_binary(Some(stale.into_os_string())),
            PathBuf::from("herdr")
        );

        let live = temporary.path().join("herdr");
        fs::write(&live, "binary").unwrap();
        assert_eq!(select_binary(Some(OsString::from(live.as_os_str()))), live);
    }

    #[test]
    fn malformed_agent_payload_is_rejected() {
        let error = parse_agent(&serde_json::json!({"pane_id": "w1:p1"})).unwrap_err();
        assert!(error.to_string().contains("agent payload"));
    }
}
