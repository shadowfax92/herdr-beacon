//! Workspace eligibility is always queried, never reconstructed from history.
mod support;
use serde_json::{json, Value};
use std::fs;
use support::cli::{agent, Fixture};
use support::PolicyServer;

#[test]
fn newly_eligible_or_recovered_workspaces_use_current_status_immediately() {
    let f = Fixture::new();
    f.live(vec![agent("d", "done", 10)]);
    f.ok("jump-unread");
    assert!(f.focuses().is_empty());
    f.policy.reply.lock().unwrap()["excluded_workspace_ids"] = json!([]);
    f.ok("jump-unread");
    assert_eq!(f.focuses(), ["p-d"]);

    let valid = f.policy.reply.lock().unwrap().clone();
    *f.policy.reply.lock().unwrap() = json!({"ok":false,"code":"not_ready","error":"starting"});
    f.live(vec![agent("a", "done", 20)]);
    assert!(!f.run("jump-unread").status.success());
    assert_eq!(f.focuses(), ["p-d"]);
    *f.policy.reply.lock().unwrap() = valid;
    f.ok("jump-unread");
    assert_eq!(f.focuses(), ["p-d", "p-a"]);
}

#[test]
fn current_canonical_locations_and_replacements_need_no_hooks() {
    let f = Fixture::new();
    f.live(vec![agent("a", "done", 10)]);
    let mut moved = agent("a", "done", 10);
    moved["pane_id"] = json!("moved");
    fs::write(
        f.root.path().join("move-on-get.json"),
        json!([moved]).to_string(),
    )
    .unwrap();
    f.ok("jump-unread");
    assert!(f.focuses().is_empty());
    f.ok("jump-unread");
    assert_eq!(f.focuses(), ["moved"]);

    f.live(vec![agent("a", "done", 20)]);
    let mut replacement = agent("a", "idle", 21);
    replacement["terminal_id"] = json!("replacement");
    fs::write(
        f.root.path().join("preflight.json"),
        json!([replacement.clone()]).to_string(),
    )
    .unwrap();
    f.ok("jump-unread");
    assert_eq!(f.focuses(), ["moved"]);
    fs::remove_file(f.root.path().join("preflight.json")).unwrap();
    f.live(vec![replacement]);
    f.ok("jump-unread");
    assert_eq!(f.focuses(), ["moved"]);
}

#[test]
fn missing_or_ambiguous_host_agents_are_never_focused() {
    for field in ["pane_id", "terminal_id"] {
        let f = Fixture::new();
        let first = agent("a", "done", 10);
        let mut second = agent("a", "done", 11);
        second["pane_id"] = json!("second");
        second["terminal_id"] = json!("second-terminal");
        second[field] = first[field].clone();
        f.live(vec![first, second]);
        assert!(!f.run("jump-unread").status.success());
        assert!(f.focuses().is_empty());
    }
    let f = Fixture::new();
    f.live(vec![agent("a", "done", 10)]);
    fs::write(f.root.path().join("preflight.json"), "[]").unwrap();
    f.ok("jump-unread");
    assert!(f.focuses().is_empty());
}

#[test]
fn a_policy_timeout_stops_focus_and_recovery_reads_current_status() {
    let f = Fixture::new();
    f.live(vec![agent("a", "done", 10)]);
    *f.policy.delay.lock().unwrap() = std::time::Duration::from_millis(2300);
    let start = std::time::Instant::now();
    assert!(!f.run("jump-unread").status.success());
    assert!(start.elapsed() < std::time::Duration::from_millis(2250));
    assert!(f.focuses().is_empty());
    *f.policy.delay.lock().unwrap() = std::time::Duration::ZERO;
    f.ok("jump-unread");
    assert_eq!(f.focuses(), ["p-a"]);
}

#[test]
fn unread_uses_api_status_even_with_legacy_pending_completion() {
    let f = Fixture::new();
    let path = f.root.path().join("beacon/state.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let legacy = json!({"version":1,"next_ordinal":1,
        "entries":{"p-a":{"pane_id":"p-a","workspace_id":"a","terminal_id":"t-a",
            "status":"done","state_change_seq":10,"ordinal":1}},"watermarks":{}})
    .to_string();
    fs::write(&path, &legacy).unwrap();
    f.live(vec![agent("a", "idle", 10)]);

    let output = f
        .command("jump-unread")
        .env("HERDR_PLUGIN_STATE_DIR", path.parent().unwrap())
        .output()
        .unwrap();
    assert!(output.status.success());

    assert!(
        f.focuses().is_empty(),
        "Alt-u selected a host-idle completion"
    );
    assert_eq!(fs::read_to_string(path).unwrap(), legacy);
}
#[test]
fn tests_that_all_modes_exclude_resolved_ids_even_when_shown() {
    for shown in [false, true] {
        for (mode, status) in [
            ("jump-unread", "done"),
            ("jump-working", "working"),
            ("jump-recent", "idle"),
            ("jump-recent-reverse", "idle"),
        ] {
            let f = Fixture::new();
            f.policy.reply.lock().unwrap()["show_excluded"] = json!(shown);
            f.live(vec![
                agent("a", status, 10),
                agent("d", status, if mode.ends_with("reverse") { 1 } else { 20 }),
            ]);
            f.ok(mode);
            assert_eq!(f.focuses(), ["p-a"], "{mode} shown={shown}");
            let requests = f.policy.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert!(requests.iter().all(|r| *r
                == json!({"cmd":"workspace_policy","version":1,"herdr_socket":"/test/host.sock"})));
        }
    }
}
#[test]
fn tests_that_invalid_wire_replies_never_focus() {
    for (field, value) in [
        ("version", json!(2)),
        ("show_excluded", json!("false")),
        ("herdr_socket", json!("/other.sock")),
        ("workspace_ids", json!([5])),
        ("excluded_workspace_ids", json!(["unknown"])),
    ] {
        let f = Fixture::new();
        f.live(vec![agent("a", "working", 8)]);
        f.policy.reply.lock().unwrap()[field] = value;
        assert!(!f.run("jump-working").status.success(), "accepted {field}");
        assert!(f.focuses().is_empty());
    }
}
#[test]
fn tests_that_unknown_workspace_and_preflight_move_never_focus() {
    let f = Fixture::new();
    f.live(vec![agent("unknown", "working", 8)]);
    f.ok("jump-working");
    assert!(f.focuses().is_empty());
    f.live(vec![agent("a", "working", 10)]);
    let mut moved = agent("a", "working", 10);
    moved["workspace_id"] = json!("d");
    fs::write(
        f.root.path().join("preflight.json"),
        json!([moved]).to_string(),
    )
    .unwrap();
    f.ok("jump-working");
    assert!(f.focuses().is_empty());
}

#[test]
fn tests_that_preflight_refresh_refuses_a_newly_excluded_target() {
    let f = Fixture::new();
    f.live(vec![agent("a", "working", 8)]);
    let mut changed = f.policy.reply.lock().unwrap().clone();
    changed["excluded_workspace_ids"] = json!(["a", "d"]);
    *f.policy.after_first.lock().unwrap() = Some(changed);
    f.ok("jump-working");
    assert!(f.focuses().is_empty());
}

#[test]
fn tests_that_malformed_oversized_and_old_agents_replies_are_closed() {
    for bytes in [
        b"{}\n".to_vec(),
        b"not json\n".to_vec(),
        b"{\"ok\":true}".to_vec(),
        vec![b' '; 256 * 1024 + 1],
        b"{\"ok\":false,\"code\":\"unsupported_version\",\"error\":\"old daemon\"}\n".to_vec(),
    ] {
        let f = Fixture::new();
        f.live(vec![agent("a", "working", 8)]);
        *f.policy.wire.lock().unwrap() = Some(bytes);
        assert!(!f.run("jump-working").status.success());
        assert!(f.focuses().is_empty());
    }
}

#[test]
fn tests_that_stopped_agents_and_missing_host_context_are_closed() {
    let f = Fixture::new();
    f.live(vec![agent("a", "working", 8)]);
    assert!(!f
        .command("jump-working")
        .env("HERDR_AGENTS_STATE", f.root.path().join("missing"))
        .output()
        .unwrap()
        .status
        .success());
    assert!(!f
        .command("jump-working")
        .env_remove("HERDR_SOCKET_PATH")
        .output()
        .unwrap()
        .status
        .success());
    assert!(f.focuses().is_empty());
}

#[test]
fn tests_that_many_candidates_share_a_bounded_number_of_policy_reads() {
    let f = Fixture::new();
    let agents = (0..400)
        .map(|n| {
            let mut a = agent(if n % 2 == 0 { "a" } else { "d" }, "working", n);
            a["pane_id"] = json!(format!("p-{n}"));
            a["terminal_id"] = json!(format!("t-{n}"));
            a
        })
        .collect();
    f.live(agents);
    f.ok("jump-working");
    assert_eq!(f.focuses(), ["p-398"]);
    assert_eq!(f.policy.requests.lock().unwrap().len(), 2);
}

#[test]
fn tests_that_discovery_uses_agents_root_and_not_the_caller_plugin_directory() {
    let f = Fixture::new();
    f.live(vec![agent("a", "working", 8)]);
    let short = tempfile::Builder::new()
        .prefix("bp-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = short.path().join("herdr/plugins/shadowfax.agents");
    let server = PolicyServer::new(&root, &["a", "d"], &["a"]);
    let output = f
        .command("jump-working")
        .env_remove("HERDR_AGENTS_STATE")
        .env("XDG_STATE_HOME", short.path())
        .env("HERDR_PLUGIN_ID", "shadowfax.beacon")
        .env(
            "HERDR_PLUGIN_CONFIG_DIR",
            f.root.path().join("irrelevant-config"),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(f.focuses().is_empty());
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    assert!(f.policy.requests.lock().unwrap().is_empty());
}

#[test]
fn tests_that_duplicate_unsorted_missing_and_invalid_sets_are_closed() {
    for (field, value) in [
        ("workspace_ids", json!(["a", "a", "d"])),
        ("workspace_ids", json!(["d", "a"])),
        ("excluded_labels", json!([""])),
        ("excluded_labels", json!(["bad\nlabel"])),
        ("ok", json!("true")),
        ("show_excluded", Value::Null),
    ] {
        let f = Fixture::new();
        f.live(vec![agent("a", "working", 8)]);
        f.policy.reply.lock().unwrap()[field] = value;
        assert!(!f.run("jump-working").status.success());
        assert!(f.focuses().is_empty());
    }
}

#[test]
fn tests_that_xdg_unset_and_empty_fall_back_but_nonempty_is_respected() {
    for xdg in [None, Some(""), Some("xdg")] {
        let f = Fixture::new();
        f.live(vec![agent("a", "working", 8)]);
        let short = tempfile::Builder::new()
            .prefix("bpx-")
            .tempdir_in("/tmp")
            .unwrap();
        let home = short.path().join("home");
        let root = match xdg {
            Some("xdg") => short.path().join("xdg/herdr/plugins/shadowfax.agents"),
            _ => home.join(".local/state/herdr/plugins/shadowfax.agents"),
        };
        let server = PolicyServer::new(&root, &["a", "d"], &["a"]);
        let mut command = f.command("jump-working");
        // Change HOME only for this child, never the owner or test process.
        command.env_remove("HERDR_AGENTS_STATE").env("HOME", &home);
        match xdg {
            None => {
                command.env_remove("XDG_STATE_HOME");
            }
            Some("") => {
                command.env("XDG_STATE_HOME", "");
            }
            Some(_) => {
                command.env("XDG_STATE_HOME", short.path().join("xdg"));
            }
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "XDG={xdg:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(f.focuses().is_empty());
        assert_eq!(server.requests.lock().unwrap().len(), 1);
        assert!(f.policy.requests.lock().unwrap().is_empty());
    }
}

#[test]
fn tests_that_explicit_agents_override_keeps_empty_and_nonempty_semantics() {
    for empty in [false, true] {
        let f = Fixture::new();
        f.live(vec![agent("a", "working", 8)]);
        let short = tempfile::Builder::new()
            .prefix("bpo-")
            .tempdir_in("/tmp")
            .unwrap();
        let server = PolicyServer::new(short.path(), &["a", "d"], &["a"]);
        let mut command = f.command("jump-working");
        command.env("XDG_STATE_HOME", short.path().join("unused"));
        if empty {
            command
                .env("HERDR_AGENTS_STATE", "")
                .current_dir(short.path());
        } else {
            command.env("HERDR_AGENTS_STATE", short.path());
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(f.focuses().is_empty());
        assert_eq!(server.requests.lock().unwrap().len(), 1);
        assert!(f.policy.requests.lock().unwrap().is_empty());
    }
}
