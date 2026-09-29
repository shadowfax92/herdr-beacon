//! Navigation through the same executable interface used by shortcuts. Every
//! destination is checked from literal host fixtures, with no production session.
mod support;
use serde_json::json;
use std::fs;
use support::cli::{agent, Fixture};

#[test]
fn unread_follows_host_acknowledgement_without_a_state_directory() {
    let f = Fixture::new();
    f.live(vec![agent("a", "done", 10)]);
    f.ok("jump-unread");
    f.ok("jump-unread");
    assert_eq!(f.focuses(), ["p-a"]);
    assert!(f.commands().contains("tab focus tab-a"));
    assert!(f
        .commands()
        .contains("notification show No other unread or blocked agents --sound none"));
    assert!(!f.root.path().join("beacon").exists());
}

#[test]
fn unread_ignores_idle_even_after_observing_new_work_or_unknown() {
    for status in ["working", "unknown", "done"] {
        let f = Fixture::new();
        f.live(vec![agent("a", status, 10)]);
        // A separate command observes the earlier status. That observation must
        // never compete with the next host-owned unread decision.
        f.ok("jump-recent");
        fs::write(f.root.path().join("commands"), "").unwrap();
        f.live(vec![agent("a", "idle", 12)]);
        f.ok("jump-unread");
        assert!(f.focuses().is_empty(), "previous status {status}");
    }
}

#[test]
fn all_modes_use_only_their_current_host_statuses() {
    for (mode, eligible) in [
        ("jump-unread", vec!["done", "blocked"]),
        ("jump-working", vec!["working", "blocked"]),
        ("jump-recent", vec!["idle", "done", "unknown"]),
        ("jump-recent-reverse", vec!["idle", "done", "unknown"]),
    ] {
        for status in ["idle", "working", "blocked", "done", "unknown"] {
            let f = Fixture::new();
            f.live(vec![agent("a", status, 10)]);
            f.ok(mode);
            let expected = if eligible.contains(&status) {
                vec!["p-a"]
            } else {
                vec![]
            };
            assert_eq!(f.focuses(), expected, "{mode} {status}");
        }
    }
}

#[test]
fn fresh_status_refuses_a_target_that_no_longer_belongs_to_the_mode() {
    for (mode, initial, changed) in [
        ("jump-unread", "done", "idle"),
        ("jump-unread", "blocked", "working"),
        ("jump-working", "working", "idle"),
        ("jump-recent", "idle", "working"),
        ("jump-recent-reverse", "done", "blocked"),
    ] {
        let f = Fixture::new();
        f.live(vec![agent("a", initial, 10)]);
        fs::write(
            f.root.path().join("preflight.json"),
            json!([agent("a", changed, 11)]).to_string(),
        )
        .unwrap();
        f.ok(mode);
        assert!(f.focuses().is_empty(), "{mode}: {initial} to {changed}");
    }
}

#[test]
fn host_errors_stop_navigation_and_tab_focus_errors_are_reported() {
    for operation in ["agent list", "agent get", "agent focus", "tab focus"] {
        let f = Fixture::new();
        f.live(vec![agent("a", "done", 10)]);
        let output = f
            .command("jump-unread")
            .env("FAKE_FAIL", operation)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{operation}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("fixture_error"));
        if matches!(operation, "agent list" | "agent get") {
            assert!(f.focuses().is_empty());
        }
    }
}

#[test]
fn empty_notification_suppression_succeeds_but_unexpected_failure_does_not() {
    for reason in [
        "disabled",
        "rate_limited",
        "no_foreground_client",
        "busy",
        "unexpected",
    ] {
        let f = Fixture::new();
        let output = f
            .command("jump-unread")
            .env("FAKE_NOTIFICATION_REASON", reason)
            .output()
            .unwrap();
        assert_eq!(output.status.success(), reason != "unexpected");
        assert!(f.focuses().is_empty());
    }
}

#[test]
fn action_context_wins_over_inherited_and_server_focus() {
    let f = Fixture::new();
    let mut newest = agent("a", "working", 20);
    newest["focused"] = json!(true);
    let mut older = agent("a", "working", 10);
    older["pane_id"] = json!("older");
    older["terminal_id"] = json!("older-terminal");
    f.live(vec![newest, older]);
    let output = f
        .command("jump-working")
        .env("HERDR_PANE_ID", "p-a")
        .env(
            "HERDR_PLUGIN_CONTEXT_JSON",
            r#"{"focused_pane_id":"older"}"#,
        )
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(f.focuses(), ["p-a"]);
}

#[test]
fn inherited_cursor_and_standalone_server_focus_are_supported() {
    for standalone in [false, true] {
        let f = Fixture::new();
        let mut newest = agent("a", "working", 20);
        newest["focused"] = json!(true);
        let mut older = agent("a", "working", 10);
        older["pane_id"] = json!("older");
        older["terminal_id"] = json!("older-terminal");
        f.live(vec![newest, older]);
        let mut command = f.command("jump-working");
        if standalone {
            command.env_remove("HERDR_PANE_ID");
        } else {
            command
                .env("HERDR_PANE_ID", "p-a")
                .env("HERDR_PLUGIN_CONTEXT_JSON", "malformed");
        }
        assert!(command.output().unwrap().status.success());
        assert_eq!(f.focuses(), ["older"]);
    }
}

#[test]
fn unread_keeps_an_idle_cursor_between_newer_blocker_and_older_completion() {
    let f = Fixture::new();
    let mut blocker = agent("a", "blocked", 30);
    blocker["pane_id"] = json!("blocker");
    blocker["terminal_id"] = json!("blocker-terminal");
    let mut older = agent("a", "done", 10);
    older["pane_id"] = json!("older");
    older["terminal_id"] = json!("older-terminal");
    f.live(vec![blocker, agent("a", "idle", 20), older]);
    let output = f
        .command("jump-unread")
        .env("HERDR_PANE_ID", "p-a")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(f.focuses(), ["older"]);
    f.ok("jump-unread");
    f.ok("jump-unread");
    assert_eq!(f.focuses(), ["older", "blocker", "blocker"]);
}

#[test]
fn a_single_current_unread_agent_is_not_another_destination() {
    for status in ["idle", "done", "blocked", "working", "unknown"] {
        let f = Fixture::new();
        f.live(vec![agent("a", status, 10)]);
        let output = f
            .command("jump-unread")
            .env("HERDR_PANE_ID", "p-a")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(f.focuses().is_empty());
    }
}

#[test]
fn recent_order_wraps_with_stable_ties_and_reverse_is_its_inverse() {
    let f = Fixture::new();
    let agents: Vec<_> = [("first", 30), ("second", 20), ("third", 20)]
        .into_iter()
        .map(|(pane, seq)| {
            let mut a = agent("a", "idle", seq);
            a["pane_id"] = json!(pane);
            a["terminal_id"] = json!(format!("terminal-{pane}"));
            a
        })
        .collect();
    for (mode, cursor, expected) in [
        ("jump-recent", "outside", "first"),
        ("jump-recent-reverse", "outside", "third"),
        ("jump-recent", "first", "second"),
        ("jump-recent", "second", "third"),
        ("jump-recent", "third", "first"),
        ("jump-recent-reverse", "second", "first"),
        ("jump-recent-reverse", "third", "second"),
        ("jump-recent-reverse", "first", "third"),
    ] {
        f.live(agents.clone());
        fs::write(f.root.path().join("commands"), "").unwrap();
        let output = f
            .command(mode)
            .env("HERDR_PANE_ID", cursor)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(f.focuses(), [expected], "{mode} {cursor}");
    }
}

#[test]
fn corrupt_or_unwritable_legacy_state_has_no_effect_on_navigation() {
    for mode in [
        "jump-unread",
        "jump-working",
        "jump-recent",
        "jump-recent-reverse",
    ] {
        let f = Fixture::new();
        let status = if mode == "jump-working" {
            "working"
        } else {
            "done"
        };
        f.live(vec![agent("a", status, 10)]);
        // A file cannot be used as a state directory. Navigation must not even
        // inspect this retired input, much less create files or repair it.
        let path = f.root.path().join("old-state");
        fs::write(&path, "corrupt old ledger").unwrap();
        let output = f
            .command(mode)
            .env("HERDR_PLUGIN_STATE_DIR", &path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(f.focuses(), ["p-a"]);
        assert_eq!(fs::read_to_string(path).unwrap(), "corrupt old ledger");
    }
}

#[test]
fn queued_legacy_hooks_are_noops_without_host_or_state_access() {
    let f = Fixture::new();
    let output = f
        .command("event")
        .env_remove("HERDR_SOCKET_PATH")
        .env("HERDR_BIN_PATH", "/usr/bin/false")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(f.commands().is_empty());
    assert!(!f.root.path().join("beacon").exists());
}
