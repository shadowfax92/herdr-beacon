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
  elif [ "$FAKE_AGENT_LIST" = "unread" ]; then
    printf '%s\n' '{"result":{"agents":[{"terminal_id":"terminal-2","agent_status":"done","workspace_id":"w1","pane_id":"w1:p2","focused":false,"state_change_seq":8}]}}'
  else
    printf '%s\n' '{"id":"fake","result":{"type":"agent_list","agents":[]}}'
  fi
elif [ "$1 $2" = "agent focus" ]; then
  case "$3" in
    w1:p1) terminal=terminal-1; sequence=10 ;;
    w1:p2) terminal=terminal-2; sequence=8 ;;
    *) exit 1 ;;
  esac
  printf '{"id":"fake","result":{"type":"agent_focus","agent":{"terminal_id":"%s","agent_status":"working","workspace_id":"w1","tab_id":"w1:t9","pane_id":"%s","focused":true,"state_change_seq":%s}}}\n' "$terminal" "$3" "$sequence"
elif [ "$1 $2" = "tab focus" ]; then
  if [ "$FAKE_TAB_FOCUS_FAIL" = 1 ]; then
    printf '%s\n' '{"error":{"code":"tab_not_found","message":"tab disappeared"}}' >&2
    exit 1
  fi
  printf '%s\n' '{"result":{"type":"tab_focus"}}'
elif [ "$1 $2" = "notification show" ]; then
  reason=${FAKE_NOTIFICATION_REASON:-shown}
  if [ "$reason" = shown ]; then shown=true; else shown=false; fi
  printf '{"id":"fake","result":{"type":"notification_show","shown":%s,"reason":"%s"}}\n' "$shown" "$reason"
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
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_PLUGIN_CONTEXT_JSON")
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
        "agent list\nagent focus w1:p2\ntab focus w1:t9\n"
    );
}

#[test]
fn working_jump_uses_action_context_before_server_focus_or_inherited_pane() {
    let temporary = tempdir().unwrap();
    let log = temporary.path().join("commands.log");
    let output = Command::new(env!("CARGO_BIN_EXE_herdr-beacon"))
        .arg("jump-working")
        .env("HERDR_BIN_PATH", fake_herdr(temporary.path()))
        .env("HERDR_PANE_ID", "w1:p1")
        .env(
            "HERDR_PLUGIN_CONTEXT_JSON",
            r#"{"focused_pane_id":"w1:p2"}"#,
        )
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
        "agent list\nagent focus w1:p1\ntab focus w1:t9\n"
    );
}

#[test]
fn failed_tab_navigation_is_reported_by_both_shortcuts() {
    for (action, agents) in [("jump-working", "working"), ("jump-unread", "unread")] {
        let temporary = tempdir().unwrap();
        let log = temporary.path().join("commands.log");
        let state_dir = temporary.path().join("state");
        let output = Command::new(env!("CARGO_BIN_EXE_herdr-beacon"))
            .arg(action)
            .env_remove("HERDR_PLUGIN_CONTEXT_JSON")
            .env("HERDR_PANE_ID", "w1:p1")
            .env("HERDR_PLUGIN_STATE_DIR", &state_dir)
            .env("HERDR_BIN_PATH", fake_herdr(temporary.path()))
            .env("FAKE_AGENT_LIST", agents)
            .env("FAKE_HERDR_LOG", &log)
            .env("FAKE_TAB_FOCUS_FAIL", "1")
            .output()
            .unwrap();

        assert!(
            !output.status.success(),
            "{action} must fail when the tab did not switch"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("tab_not_found"));
        assert_eq!(
            fs::read_to_string(log).unwrap(),
            "agent list\nagent focus w1:p2\ntab focus w1:t9\n"
        );
        if action == "jump-unread" {
            // No successful navigation means the jump itself must not acknowledge it.
            assert_eq!(
                StateStore::new(state_dir)
                    .read()
                    .unwrap()
                    .newest()
                    .unwrap()
                    .pane_id,
                "w1:p2"
            );
        }
    }
}

#[test]
fn suppressed_empty_queue_notifications_are_successful_no_ops() {
    for action in ["jump-unread", "jump-working"] {
        for reason in ["rate_limited", "disabled", "busy", "no_foreground_client"] {
            let temporary = tempdir().unwrap();
            let output = Command::new(env!("CARGO_BIN_EXE_herdr-beacon"))
                .arg(action)
                .env("HERDR_BIN_PATH", fake_herdr(temporary.path()))
                .env("HERDR_PLUGIN_STATE_DIR", temporary.path().join("state"))
                .env("FAKE_HERDR_LOG", temporary.path().join("commands.log"))
                .env("FAKE_AGENT_LIST", "empty")
                .env("FAKE_NOTIFICATION_REASON", reason)
                .output()
                .unwrap();

            assert!(
                output.status.success(),
                "{action}/{reason}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn unknown_notification_failures_are_not_silenced() {
    let temporary = tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_herdr-beacon"))
        .arg("jump-working")
        .env("HERDR_BIN_PATH", fake_herdr(temporary.path()))
        .env("FAKE_HERDR_LOG", temporary.path().join("commands.log"))
        .env("FAKE_AGENT_LIST", "empty")
        .env("FAKE_NOTIFICATION_REASON", "unexpected_failure")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected_failure"));
}
