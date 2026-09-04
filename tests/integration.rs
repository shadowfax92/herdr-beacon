use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use herdr_beacon::state::StateStore;
use tempfile::tempdir;

fn fake_herdr(root: &Path) -> PathBuf {
    let path = root.join("herdr");
    fs::write(
        &path,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_HERDR_LOG"
if [ "$1 $2" = "agent get" ]; then
  printf '%s\n' '{"id":"fake","result":{"type":"agent_get","agent":{"terminal_id":"terminal-1","agent_status":"done","workspace_id":"w1","pane_id":"w1:p1","focused":false,"state_change_seq":8}}}'
elif [ "$1 $2" = "agent list" ]; then
  if [ "$FAKE_AGENT_LIST" = "working" ]; then
    printf '%s\n' '{"id":"fake","result":{"type":"agent_list","agents":[{"terminal_id":"terminal-1","agent_status":"working","workspace_id":"w1","pane_id":"w1:p1","focused":true,"state_change_seq":10},{"terminal_id":"terminal-2","agent_status":"working","workspace_id":"w1","pane_id":"w1:p2","focused":false,"state_change_seq":8}]}}'
  else
    printf '%s\n' '{"id":"fake","result":{"type":"agent_list","agents":[]}}'
  fi
elif [ "$1 $2" = "agent focus" ]; then
  printf '%s\n' '{"id":"fake","result":{"type":"agent_focus","agent":{"terminal_id":"terminal-2","agent_status":"working","workspace_id":"w1","pane_id":"w1:p2","focused":true,"state_change_seq":8}}}'
elif [ "$1 $2" = "notification show" ]; then
  printf '%s\n' '{"id":"fake","result":{"type":"notification_show","shown":true,"reason":"shown"}}'
else
  printf '%s\n' '{"id":"fake","error":{"code":"unexpected","message":"unexpected command"}}' >&2
  exit 1
fi
"#,
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn event_command_consumes_the_herdr_hook_environment() {
    let temporary = tempdir().unwrap();
    let state_dir = temporary.path().join("state");
    let log = temporary.path().join("commands.log");
    let output = Command::new(env!("CARGO_BIN_EXE_herdr-beacon"))
        .arg("event")
        .env("HERDR_BIN_PATH", fake_herdr(temporary.path()))
        .env("HERDR_PLUGIN_STATE_DIR", &state_dir)
        .env("HERDR_PLUGIN_EVENT", "pane.agent_status_changed")
        .env(
            "HERDR_PLUGIN_EVENT_JSON",
            r#"{"event":"pane_agent_status_changed","data":{"type":"pane_agent_status_changed","pane_id":"w1:p1","workspace_id":"w1","agent_status":"done"}}"#,
        )
        .env("FAKE_HERDR_LOG", &log)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        StateStore::new(state_dir)
            .read()
            .unwrap()
            .newest()
            .unwrap()
            .pane_id,
        "w1:p1"
    );
    assert_eq!(fs::read_to_string(log).unwrap(), "agent get w1:p1\n");
}

#[test]
fn empty_jump_requests_an_explicitly_soundless_notification() {
    let temporary = tempdir().unwrap();
    let log = temporary.path().join("commands.log");
    let output = Command::new(env!("CARGO_BIN_EXE_herdr-beacon"))
        .arg("jump-unread")
        .env("HERDR_BIN_PATH", fake_herdr(temporary.path()))
        .env("HERDR_PLUGIN_STATE_DIR", temporary.path().join("state"))
        .env("FAKE_HERDR_LOG", &log)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(log).unwrap(),
        "agent list\nnotification show No unread agents --sound none\n"
    );
}

#[test]
fn working_jump_focuses_the_next_active_turn() {
    let temporary = tempdir().unwrap();
    let log = temporary.path().join("commands.log");
    let output = Command::new(env!("CARGO_BIN_EXE_herdr-beacon"))
        .arg("jump-working")
        .env("HERDR_BIN_PATH", fake_herdr(temporary.path()))
        .env("FAKE_AGENT_LIST", "working")
        .env("FAKE_HERDR_LOG", &log)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(log).unwrap(),
        "agent list\nagent focus w1:p2\n"
    );
}
