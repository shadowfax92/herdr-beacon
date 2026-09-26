//! End-to-end Beacon CLI regressions with private Agents sockets and a fake
//! Herdr executable. Nothing addresses the owner's session or installed state.
mod support;
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};
use support::PolicyServer;
use tempfile::{tempdir, TempDir};

struct Fixture {
    root: TempDir,
    policy: PolicyServer,
}
impl Fixture {
    fn new() -> Self {
        let root = tempdir().unwrap();
        let policy = PolicyServer::new(&root.path().join("agents"), &["a", "d"], &["d"]);
        fs::write(
            root.path().join("herdr"),
            r#"#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
r=Path(os.environ['FIXTURE_ROOT']); args=sys.argv[1:]
with (r/'commands').open('a') as f: f.write(' '.join(args)+'\n')
agents=json.loads((r/'agents.json').read_text())
if args[:2]==['agent','list']: result={'agents':agents}
elif args[:2]==['agent','get']:
    if (r/'preflight.json').exists(): agents=json.loads((r/'preflight.json').read_text())
    matches=[a for a in agents if a['pane_id']==args[2]]
    if not matches:
        print(json.dumps({'error':{'code':'agent_not_found','message':'missing'}}));sys.exit(1)
    result={'agent':matches[0]}
elif args[:2]==['agent','focus']:
    matches=[a for a in agents if a['pane_id']==args[2]]
    assert matches
    result={'agent':dict(matches[0],focused=True)}
else: result={'shown':True}
print(json.dumps({'result':result}))
"#,
        )
        .unwrap();
        fs::set_permissions(root.path().join("herdr"), fs::Permissions::from_mode(0o755)).unwrap();
        let f = Self { root, policy };
        f.live(vec![]);
        f
    }
    fn live(&self, agents: Vec<Value>) {
        fs::write(
            self.root.path().join("agents.json"),
            json!(agents).to_string(),
        )
        .unwrap();
    }
    fn command(&self, command: &str) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_herdr-beacon"));
        c.arg(command)
            .env("HERDR_BIN_PATH", self.root.path().join("herdr"))
            .env("HERDR_PLUGIN_STATE_DIR", self.root.path().join("beacon"))
            .env("HERDR_AGENTS_STATE", self.root.path().join("agents"))
            .env("HERDR_SOCKET_PATH", "/test/./host.sock")
            .env("FIXTURE_ROOT", self.root.path())
            .env("HERDR_PANE_ID", "outside")
            .env_remove("HERDR_PLUGIN_CONTEXT_JSON");
        if command == "event" {
            c.env("HERDR_PLUGIN_EVENT","pane.agent_status_changed").env("HERDR_PLUGIN_EVENT_JSON",r#"{"event":"pane_agent_status_changed","data":{"type":"pane_agent_status_changed","pane_id":"p-d"}}"#);
        }
        c
    }
    fn run(&self, command: &str) -> Output {
        self.command(command).output().unwrap()
    }
    fn ok(&self, c: &str) {
        let o = self.run(c);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    fn focuses(&self) -> Vec<String> {
        fs::read_to_string(self.root.path().join("commands"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.strip_prefix("agent focus ").map(str::to_string))
            .collect()
    }
    fn state(&self) -> Value {
        serde_json::from_slice(&fs::read(self.root.path().join("beacon/state.json")).unwrap())
            .unwrap()
    }
}
fn agent(ws: &str, status: &str, seq: u64) -> Value {
    json!({"pane_id":format!("p-{ws}"),"workspace_id":ws,"terminal_id":format!("t-{ws}"),"tab_id":format!("tab-{ws}"),"agent_status":status,"state_change_seq":seq,"focused":false})
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
            assert!(f.state()["entries"]
                .as_object()
                .unwrap()
                .values()
                .all(|v| v["workspace_id"] != "d"));
            let requests = f.policy.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert!(requests.iter().all(|r| *r
                == json!({"cmd":"workspace_policy","version":1,"herdr_socket":"/test/host.sock"})));
        }
    }
}
#[test]
fn tests_that_hook_exclusion_and_policy_removal_baseline_completion() {
    let f = Fixture::new();
    f.live(vec![agent("d", "done", 8)]);
    f.ok("event");
    assert!(f.state()["entries"].as_object().unwrap().is_empty());
    {
        let mut p = f.policy.reply.lock().unwrap();
        p["excluded_labels"] = json!([]);
        p["excluded_workspace_ids"] = json!([]);
    }
    f.ok("jump-unread");
    assert!(f.focuses().is_empty());
    f.live(vec![agent("d", "done", 10)]);
    f.ok("jump-unread");
    assert_eq!(f.focuses(), ["p-d"]);
}
#[test]
fn tests_that_policy_failure_is_closed_and_recovery_does_not_replay() {
    let f = Fixture::new();
    f.live(vec![agent("a", "done", 8)]);
    let valid = f.policy.reply.lock().unwrap().clone();
    *f.policy.reply.lock().unwrap() = json!({"ok":false,"code":"not_ready","error":"starting"});
    for mode in [
        "event",
        "jump-unread",
        "jump-working",
        "jump-recent",
        "jump-recent-reverse",
    ] {
        assert!(!f.run(mode).status.success());
    }
    assert!(f.focuses().is_empty());
    *f.policy.reply.lock().unwrap() = valid;
    f.ok("jump-unread");
    assert!(f.focuses().is_empty());
    f.live(vec![agent("a", "done", 10)]);
    f.ok("jump-unread");
    assert_eq!(f.focuses(), ["p-a"]);
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
fn tests_that_reset_at_same_host_path_baselines_current_completion() {
    let f = Fixture::new();
    f.live(vec![agent("a", "working", 80)]);
    f.ok("event");
    f.live(vec![agent("a", "done", 2)]);
    f.ok("jump-unread");
    assert!(f.focuses().is_empty());
    f.live(vec![agent("a", "done", 4)]);
    f.ok("jump-unread");
    assert_eq!(f.focuses(), ["p-a"]);
}

#[test]
fn tests_that_refused_preflight_keeps_other_pending_completions() {
    let f = Fixture::new();
    {
        let mut p = f.policy.reply.lock().unwrap();
        p["workspace_ids"] = json!(["a", "b", "d"]);
    }
    f.live(vec![agent("a", "done", 10), agent("b", "done", 8)]);
    let mut changed = agent("a", "done", 10);
    changed["terminal_id"] = json!("replacement");
    fs::write(
        f.root.path().join("preflight.json"),
        json!([changed]).to_string(),
    )
    .unwrap();
    f.ok("jump-unread");
    assert!(f.focuses().is_empty());
    fs::remove_file(f.root.path().join("preflight.json")).unwrap();
    f.ok("jump-unread");
    assert_eq!(f.focuses(), ["p-b"]);
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
fn tests_that_show_hide_never_reenters_but_rename_and_move_do() {
    for mode in ["rename", "move", "recreate"] {
        let f = Fixture::new();
        f.live(vec![agent("d", "done", 8)]);
        f.ok("event");
        f.policy.reply.lock().unwrap()["show_excluded"] = json!(true);
        f.ok("jump-unread");
        assert!(f.focuses().is_empty());
        let mut current = agent("d", "done", 10);
        match mode {
            "rename" => f.policy.reply.lock().unwrap()["excluded_workspace_ids"] = json!([]),
            "move" => {
                current["workspace_id"] = json!("a");
                current["pane_id"] = json!("new-address");
            }
            _ => {
                current["terminal_id"] = json!("new-terminal");
            }
        }
        f.live(vec![current.clone()]);
        f.ok("jump-unread");
        assert!(f.focuses().is_empty());
        current["state_change_seq"] = json!(12);
        f.live(vec![current]);
        f.ok("jump-unread");
        if mode == "recreate" {
            assert!(f.focuses().is_empty());
        } else {
            assert_eq!(f.focuses().len(), 1);
        }
    }
}

#[test]
fn tests_that_legacy_migration_preserves_ambiguous_pending_and_acknowledgements() {
    for cleared in [None, Some(10)] {
        let f = Fixture::new();
        fs::create_dir_all(f.root.path().join("beacon")).unwrap();
        let mut mark = json!({"terminal_id":"t-a","state_change_seq":10});
        if let Some(seq) = cleared {
            mark["cleared_through"] = json!(seq);
        }
        fs::write(f.root.path().join("beacon/state.json"),json!({"version":1,"next_ordinal":1,
            "entries":{"p-a":{"pane_id":"p-a","workspace_id":"a","terminal_id":"t-a","status":"done","state_change_seq":10,"ordinal":1}},
            "watermarks":{"p-a":mark}}).to_string()).unwrap();
        f.live(vec![agent("a", "idle", 10)]);
        f.ok("jump-unread");
        assert_eq!(f.focuses().len(), usize::from(cleared.is_none()));
        assert_eq!(f.state()["version"], 2);
        assert_eq!(f.state()["watermarks"]["p-a"]["cleared_through"], 10);
    }
}

#[test]
fn tests_that_partial_v2_or_unknown_legacy_fields_are_not_overwritten() {
    for state in [
        json!({"version":2,"next_ordinal":0,"entries":{},"watermarks":{}}),
        json!({"version":1,"next_ordinal":0,"entries":{},"watermarks":{},"unrecognized":true}),
    ] {
        let f = Fixture::new();
        let path = f.root.path().join("beacon/state.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, state.to_string()).unwrap();
        f.live(vec![agent("a", "working", 1)]);
        assert!(!f.run("jump-working").status.success());
        assert!(f.focuses().is_empty());
        assert_eq!(fs::read_to_string(path).unwrap(), state.to_string());
    }
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
fn tests_that_slow_reply_has_one_deadline_and_recovery_baselines() {
    let f = Fixture::new();
    f.live(vec![agent("a", "done", 8)]);
    *f.policy.delay.lock().unwrap() = std::time::Duration::from_millis(2300);
    let start = std::time::Instant::now();
    assert!(!f.run("jump-unread").status.success());
    assert!(start.elapsed() < std::time::Duration::from_millis(2250));
    assert!(f.focuses().is_empty());
    *f.policy.delay.lock().unwrap() = std::time::Duration::ZERO;
    f.ok("jump-unread");
    assert!(f.focuses().is_empty());
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
fn tests_that_cli_unread_regressions_preserve_passive_move_history() {
    for initial in ["working", "done"] {
        let f = Fixture::new();
        let mut a = agent("a", initial, 9);
        f.live(vec![a.clone()]);
        f.ok("event");
        a["agent_status"] = json!("idle");
        a["state_change_seq"] = json!(10);
        f.live(vec![a.clone()]);
        f.ok("event");
        a["pane_id"] = json!("moved");
        f.live(vec![a.clone()]);
        // An old-address status hook must not be interpreted as acknowledgement.
        f.ok("event");
        f.ok("jump-unread");
        assert_eq!(f.focuses(), ["moved"]);
        f.ok("event");
        f.ok("jump-unread");
        assert_eq!(f.focuses(), ["moved"]);
    }
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
fn tests_that_unknown_membership_recovery_baselines_without_affecting_other_workspaces() {
    let f = Fixture::new();
    f.live(vec![agent("unknown", "done", 10), agent("a", "done", 8)]);
    f.ok("jump-unread");
    assert_eq!(f.focuses(), ["p-a"]);
    f.policy.reply.lock().unwrap()["workspace_ids"] = json!(["a", "d", "unknown"]);
    f.ok("jump-unread");
    assert_eq!(f.focuses(), ["p-a"]);
    f.live(vec![agent("unknown", "done", 12)]);
    f.ok("jump-unread");
    assert_eq!(f.focuses(), ["p-a", "p-unknown"]);
}

#[test]
fn tests_that_topology_hooks_and_delayed_aliases_use_only_canonical_location() {
    let f = Fixture::new();
    f.live(vec![agent("a", "done", 10)]);
    f.ok("event");
    let mut moved = agent("a", "done", 10);
    moved["workspace_id"] = json!("d");
    moved["pane_id"] = json!("moved");
    f.live(vec![moved]);
    for (name, data) in [
        (
            "pane.moved",
            json!({"previous_pane_id":"old","pane":{"pane_id":"p-a","workspace_id":"a","terminal_id":"t-a"}}),
        ),
        ("workspace.renamed", json!({"workspace_id":"d"})),
        ("workspace.created", json!({"workspace_id":"d"})),
        ("workspace.closed", json!({"workspace_id":"a"})),
    ] {
        let tag = name.replace('.', "_");
        let mut data = data;
        data["type"] = json!(tag);
        let o = f
            .command("event")
            .env("HERDR_PLUGIN_EVENT", name)
            .env(
                "HERDR_PLUGIN_EVENT_JSON",
                json!({"event":tag,"data":data}).to_string(),
            )
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert!(f.state()["entries"].as_object().unwrap().is_empty());
        assert!(f.state()["watermarks"].as_object().unwrap().is_empty());
    }
}

#[test]
fn tests_that_exclusion_survives_empty_snapshots_and_recreated_workspaces() {
    let f = Fixture::new();
    f.live(vec![agent("d", "done", 10)]);
    f.ok("event");
    f.live(vec![]);
    f.policy.reply.lock().unwrap()["workspace_ids"] = json!(["a"]);
    f.policy.reply.lock().unwrap()["excluded_workspace_ids"] = json!([]);
    f.ok("jump-unread");
    let mut returning = agent("d", "done", 20);
    returning["workspace_id"] = json!("a");
    returning["pane_id"] = json!("returned");
    f.live(vec![returning.clone()]);
    f.ok("jump-unread");
    assert!(f.focuses().is_empty());
    returning["state_change_seq"] = json!(22);
    f.live(vec![returning]);
    f.ok("jump-unread");
    assert_eq!(f.focuses(), ["returned"]);
    f.policy.reply.lock().unwrap()["workspace_ids"] = json!(["a", "new-d"]);
    f.policy.reply.lock().unwrap()["excluded_workspace_ids"] = json!(["new-d"]);
    f.live(vec![agent("new-d", "done", 100)]);
    f.ok("jump-unread");
    assert_eq!(f.focuses(), ["returned"]);
}

#[test]
fn tests_that_config_removal_baselines_newly_resolved_history() {
    let f = Fixture::new();
    // The configured label had no live workspace at the previous invocation.
    f.policy.reply.lock().unwrap()["excluded_workspace_ids"] = json!([]);
    f.live(vec![]);
    f.ok("event");
    f.policy.reply.lock().unwrap()["excluded_labels"] = json!([]);
    f.live(vec![agent("a", "done", 20)]);
    f.ok("jump-unread");
    assert!(f.focuses().is_empty());
    f.live(vec![agent("a", "done", 22)]);
    f.ok("jump-unread");
    assert_eq!(f.focuses(), ["p-a"]);
}

#[test]
fn tests_that_recreated_destination_does_not_inherit_another_terminal_pending_entry() {
    let f = Fixture::new();
    fs::create_dir_all(f.root.path().join("beacon")).unwrap();
    fs::write(f.root.path().join("beacon/state.json"),json!({"version":1,"next_ordinal":1,
        "entries":{"p-a":{"pane_id":"p-a","workspace_id":"a","terminal_id":"departed","status":"done","state_change_seq":100,"ordinal":1}},
        "watermarks":{"old-address":{"terminal_id":"t-a","state_change_seq":10,"cleared_through":10}}}).to_string()).unwrap();
    f.live(vec![agent("a", "done", 10)]);
    f.ok("jump-unread");
    assert!(f.focuses().is_empty());
}
