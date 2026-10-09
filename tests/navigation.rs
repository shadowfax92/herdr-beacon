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
fn unread_ignores_unmarked_idle_even_after_observing_new_work_or_unknown() {
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

fn mark_unread(f: &Fixture, entries: &[(&str, &str)]) {
    f.policy.reply.lock().unwrap()["unread"] = entries
        .iter()
        .map(|(pane, terminal)| json!({"pane_id":pane,"terminal_id":terminal}))
        .collect();
}

#[test]
fn unread_reaches_completions_agents_marks_after_herdr_reads_their_tab() {
    // Herdr reads every pane in a visible tab, so a peer that finished behind a
    // zoomed pane is already idle. Only the Agents mark says nobody saw it.
    for status in ["idle", "unknown", "done"] {
        let f = Fixture::new();
        f.live(vec![agent("a", status, 10)]);
        mark_unread(&f, &[("p-a", "t-a")]);
        f.ok("jump-unread");
        assert_eq!(f.focuses(), ["p-a"], "{status}");
    }
}

#[test]
fn agents_marks_need_the_same_terminal_and_an_agent_at_rest() {
    for (status, terminal) in [("idle", "replaced"), ("working", "t-a")] {
        let f = Fixture::new();
        f.live(vec![agent("a", status, 10)]);
        mark_unread(&f, &[("p-a", terminal)]);
        f.ok("jump-unread");
        assert!(f.focuses().is_empty(), "{status} {terminal}");
    }
}

#[test]
fn agents_marks_do_not_change_working_or_recent_modes() {
    for (mode, status) in [("jump-working", "idle"), ("jump-recent", "working")] {
        let f = Fixture::new();
        f.live(vec![agent("a", status, 10)]);
        mark_unread(&f, &[("p-a", "t-a")]);
        f.ok(mode);
        assert!(f.focuses().is_empty(), "{mode} {status}");
    }
}

#[test]
fn the_fresh_recheck_refuses_a_completion_agents_stopped_marking() {
    let f = Fixture::new();
    f.live(vec![agent("a", "idle", 10)]);
    mark_unread(&f, &[("p-a", "t-a")]);
    // The pane was focused between selection and the pre-focus policy read.
    let mut read = f.policy.reply.lock().unwrap().clone();
    read["unread"] = json!([]);
    *f.policy.after_first.lock().unwrap() = Some(read);
    f.ok("jump-unread");
    assert!(f.focuses().is_empty());
}

#[test]
fn unread_visits_each_marked_peer_after_herdr_reads_their_shared_tab() {
    // Zoomed peers share one tab: focusing either makes Herdr report both
    // idle, while Agents keeps the mark on the peer nobody has looked at.
    let f = Fixture::new();
    let peer = |pane: &str, seq| {
        let mut peer = agent("a", "idle", seq);
        peer["pane_id"] = json!(pane);
        peer["terminal_id"] = json!(format!("t-{pane}"));
        peer["tab_id"] = json!("tab-shared");
        peer
    };
    f.live(vec![peer("older", 10), peer("newer", 20)]);
    mark_unread(&f, &[("newer", "t-newer"), ("older", "t-older")]);
    f.ok("jump-unread");
    assert_eq!(f.focuses(), ["newer"]);
    // Agents observed the focus and cleared only that pane's mark.
    mark_unread(&f, &[("older", "t-older")]);
    let from = |pane: &str| {
        let output = f
            .command("jump-unread")
            .env("HERDR_PANE_ID", pane)
            .output()
            .unwrap();
        assert!(output.status.success());
    };
    from("newer");
    assert_eq!(f.focuses(), ["newer", "older"]);
    mark_unread(&f, &[]);
    from("older");
    assert_eq!(f.focuses(), ["newer", "older"]);
    assert!(f
        .commands()
        .contains("notification show No other unread or blocked agents"));
}

fn peer(pane: &str, status: &str, seq: u64) -> serde_json::Value {
    let mut peer = agent("a", status, seq);
    peer["pane_id"] = json!(pane);
    peer["terminal_id"] = json!(format!("t-{pane}"));
    peer
}

/// Agents' published sidebar rows, top to bottom.
fn sidebar(f: &Fixture, panes: &[&str]) {
    f.policy.reply.lock().unwrap()["order"] = panes
        .iter()
        .map(|pane| json!({"pane_id":pane,"terminal_id":format!("t-{pane}")}))
        .collect();
}

fn press(f: &Fixture, mode: &str, cursor: &str) {
    let output = f
        .command(mode)
        .env("HERDR_PANE_ID", cursor)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn every_mode_walks_the_sidebar_rows_not_herdr_recency() {
    for (mode, status) in [
        ("jump-recent", "idle"),
        ("jump-working", "working"),
        ("jump-unread", "done"),
    ] {
        let f = Fixture::new();
        // Herdr recency says x, y, z; the sidebar shows z, x, y.
        f.live(vec![
            peer("x", status, 30),
            peer("y", status, 20),
            peer("z", status, 10),
        ]);
        sidebar(&f, &["z", "x", "y"]);
        for cursor in ["outside", "z", "x", "y"] {
            press(&f, mode, cursor);
        }
        // Focusing reads a completion, so unread has nothing left to wrap to.
        let expected: &[&str] = if mode == "jump-unread" {
            &["z", "x", "y"]
        } else {
            &["z", "x", "y", "z"]
        };
        assert_eq!(f.focuses(), expected, "{mode}");
    }
}

#[test]
fn reverse_walks_up_the_sidebar_and_starts_at_the_bottom() {
    let f = Fixture::new();
    f.live(vec![
        peer("x", "idle", 30),
        peer("y", "idle", 20),
        peer("z", "idle", 10),
    ]);
    sidebar(&f, &["z", "x", "y"]);
    for cursor in ["outside", "y", "x", "z"] {
        press(&f, "jump-recent-reverse", cursor);
    }
    assert_eq!(f.focuses(), ["y", "x", "z", "y"]);
}

#[test]
fn agents_missing_from_the_sidebar_order_follow_it_by_recency() {
    let f = Fixture::new();
    f.live(vec![
        peer("a", "idle", 30),
        peer("b", "idle", 10),
        peer("c", "idle", 20),
    ]);
    sidebar(&f, &["b"]);
    for cursor in ["outside", "b", "a"] {
        press(&f, "jump-recent", cursor);
    }
    assert_eq!(f.focuses(), ["b", "a", "c"]);
}

#[test]
fn a_replaced_terminal_does_not_inherit_its_panes_row() {
    let f = Fixture::new();
    // Herdr recency prefers x; its row belonged to an older terminal.
    f.live(vec![peer("x", "idle", 30), peer("y", "idle", 10)]);
    sidebar(&f, &["x", "y"]);
    f.policy.reply.lock().unwrap()["order"][0]["terminal_id"] = json!("t-old");
    press(&f, "jump-recent", "outside");
    assert_eq!(f.focuses(), ["y"]);
}

#[test]
fn unread_anchor_continues_below_the_read_row() {
    let f = Fixture::new();
    // Herdr recency would go from b back up to a; the sidebar continues down.
    f.live(vec![
        peer("a", "done", 10),
        peer("b", "idle", 20),
        peer("c", "done", 30),
    ]);
    sidebar(&f, &["a", "b", "c"]);
    press(&f, "jump-unread", "b");
    assert_eq!(f.focuses(), ["c"]);
}
