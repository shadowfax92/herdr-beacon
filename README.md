<div align="center">

# 🔦 Beacon

**Navigate agents by attention, working turns, or recent activity in Herdr.**

[![Herdr 0.7.5+](https://img.shields.io/badge/Herdr-0.7.5%2B-6c71c4)](https://herdr.dev)
[![Rust](https://img.shields.io/badge/built%20with-Rust-b7410e)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

</div>

Beacon remembers background agents that finish and keeps blocked requests within reach.

| Shortcut | Agents included | Order |
| --- | --- | --- |
| `Alt-u` | Unread completions and all blocked agents, including ones already viewed | Most recent activity first |
| `Alt-o` | Currently working agents | Most recently started turn first |
| `Alt-'` | Every live agent: working, blocked, done, idle, and unknown | Most recent activity first |

Repeated presses advance from the current agent and wrap at the end; starting outside the eligible set selects its newest agent. Activity means Herdr's latest lifecycle transition (`state_change_seq`), not how often you focus a pane. Ties use pane ID, and each press refreshes the live order.

- A new completion (`idle` or `done`) becomes unread when its state-change sequence advances beyond Beacon's last observation or acknowledgement.
- An agent first encountered as `idle` establishes a baseline, not an unread entry. Existing `done` agents can seed the queue.
- `blocked` becomes unread on a status hook or a newly observed transition. All live blockers are also eligible for `Alt-u` independently of unread state, including blockers found at startup or already viewed.
- A pane-focus event, successful unread jump, or invoking `Alt-u` from a pane marks its Beacon entry read.
- Closed, exited, running, unknown, and missing agents are removed from the unread queue automatically.
- With no other unread or blocked destination, `Alt-u` requests a soundless notification. Herdr may suppress it without making the shortcut fail.
- Working and recent-activity navigation read the live agent list without directly changing queue state; normal focus hooks still acknowledge viewed work.
- From outside the working set, `Alt-o` starts with the newest turn; repeated presses advance and wrap.

## Install

Requires macOS or Linux (including WSL), Herdr 0.7.5 or newer, and a Rust toolchain.

On Windows, run Herdr and this plugin inside WSL. Native Windows is not supported.

```sh
herdr plugin install shadowfax92/herdr-beacon --yes
herdr plugin action invoke shadowfax.beacon.install-keybindings
```

Herdr builds and enables Beacon from its locked Rust manifest. The second command adds these conflict-checked blocks to `~/.config/herdr/config.toml`, backs up an existing config, and reloads Herdr:

```toml
[[keys.command]]
key = "alt+u"
type = "plugin_action"
command = "shadowfax.beacon.jump-unread"
description = "Cycle through unread or blocked agents"

[[keys.command]]
key = "alt+o"
type = "plugin_action"
command = "shadowfax.beacon.jump-working"
description = "Cycle through working agents"

[[keys.command]]
key = "alt+quote"
type = "plugin_action"
command = "shadowfax.beacon.jump-recent"
description = "Cycle through all agents by recent activity"
```

It preserves unrelated configuration, is byte-for-byte idempotent, and refuses to replace any built-in or custom binding already using `Alt-u`, `Alt-o`, or `Alt-'`.

To work on a local checkout instead:

```sh
cargo build --release --locked
herdr plugin link .
herdr plugin action invoke shadowfax.beacon.install-keybindings
```

## How it works

Herdr runs Beacon on agent-status, pane-focus, close, exit, detection, and move events. Beacon validates each event against the live `herdr agent` record, then stores only pane identity, attention status, and ordering metadata in its private plugin state directory.

Herdr hook processes can finish out of order. Beacon orders entries with Herdr's `state_change_seq` and keeps per-terminal acknowledgement watermarks so a late completion hook cannot resurrect work that was already focused. Live API reads and state updates share a filesystem lock; state is saved in atomic private files. Existing v1 state files remain compatible.

Before every unread jump, Beacon reconciles the queue with `herdr agent list`. This recovers newer settled transitions even when a hook was missed, prunes stale entries, and rebases ordering after a Herdr server sequence reset. An unchanged `done` or `blocked` status cannot override Beacon's acknowledgement. Fresh idle/blocked snapshots are baselined rather than guessed to be unread.

Working and recent-activity jumps read the same live agent list but never touch persisted queue state. Herdr's `state_change_seq` supplies the recency order. The action's `HERDR_PLUGIN_CONTEXT_JSON.focused_pane_id` (or `HERDR_PANE_ID`) supplies the cycle cursor, rather than another client's server focus. Standalone commands without pane context fall back to API focus.

All three shortcuts focus the agent and then explicitly focus its returned tab. Herdr 0.9's `agent focus` alone can report success without switching the visible TUI tab. The explicit tab focus also affects other TUI clients attached to the same server.

In Herdr 0.9, API `done` uses server-side seen state, while each TUI tracks viewed completions independently. Beacon owns one shared unread queue per plugin state directory; it does not read those client-private acknowledgements. Merely viewing a visible split may therefore clear a sidebar badge without clearing Beacon's queue. No Herdr fork changes are required. See the [official API semantics](https://herdr.dev/docs/agent-automation/#choose-the-control-surface).

## Inspect and troubleshoot

```sh
herdr plugin list
herdr plugin action list --plugin shadowfax.beacon
herdr plugin log list --plugin shadowfax.beacon --limit 20
```

Beacon has no popup or queue panel in v1. It also has no manual mark-unread or defer command.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

To verify actual TUI navigation with an installed Herdr (each run creates and stops its own isolated test server; set `BEACON_TEST_SHORTCUT=1` to send the real shortcut):

```sh
python3 tests/real_tui_navigation.py target/release/herdr-beacon jump-working tab
python3 tests/real_tui_navigation.py target/release/herdr-beacon jump-unread tab
python3 tests/real_tui_navigation.py target/release/herdr-beacon jump-working workspace
python3 tests/real_tui_navigation.py target/release/herdr-beacon jump-unread workspace
python3 tests/real_tui_navigation.py target/release/herdr-beacon jump-unread workspace blocked
python3 tests/real_tui_navigation.py target/release/herdr-beacon jump-recent tab idle
python3 tests/real_tui_navigation.py target/release/herdr-beacon jump-recent workspace blocked
```

## Remove

Delete Beacon's `[[keys.command]]` blocks, then run:

```sh
herdr server reload-config
herdr plugin uninstall shadowfax.beacon
```

Use `herdr plugin unlink shadowfax.beacon` instead when Beacon was linked from a local checkout.

## License

[MIT](LICENSE)
