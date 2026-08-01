use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn cli_exposes_only_the_supported_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_herdr-beacon"))
        .arg("--help")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();

    assert!(output.status.success());
    assert!(stdout.contains("event"));
    assert!(stdout.contains("jump-unread"));
    assert!(stdout.contains("install-keybindings"));
}

#[test]
fn manifest_declares_actions_hooks_and_locked_build() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(root.join("herdr-plugin.toml")).unwrap();
    let value = manifest.parse::<toml_edit::DocumentMut>().unwrap();

    assert_eq!(value["min_herdr_version"].as_str(), Some("0.7.5"));
    let build = value["build"].as_array_of_tables().unwrap();
    assert_eq!(build.len(), 1);
    assert!(build.get(0).unwrap()["command"]
        .as_array()
        .unwrap()
        .iter()
        .any(|part| part.as_str() == Some("--locked")));

    let actions = value["actions"].as_array_of_tables().unwrap();
    let action_ids = actions
        .iter()
        .map(|action| action["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(action_ids, ["jump-unread", "install-keybindings"]);

    let events = value["events"].as_array_of_tables().unwrap();
    let event_names = events
        .iter()
        .map(|event| event["on"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        event_names,
        [
            "pane.agent_status_changed",
            "pane.focused",
            "pane.closed",
            "pane.exited",
            "pane.agent_detected",
            "pane.moved",
        ]
    );
}
