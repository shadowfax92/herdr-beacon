<div align="center">

# 🔦 Beacon

**Jump to unread agents or cycle through active turns in Herdr.**

[![Herdr 0.7.5+](https://img.shields.io/badge/Herdr-0.7.5%2B-6c71c4)](https://herdr.dev)
[![Rust](https://img.shields.io/badge/built%20with-Rust-b7410e)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

</div>

Beacon remembers background agents that finish or need input. Press `Alt-u` to focus the newest unread agent; press it again for the next one. Press `Alt-o` to cycle through agents that are currently working, ordered by the most recently started turn.

- A new completion (`idle` or `done`) becomes unread when its state-change sequence advances beyond Beacon's last observation or acknowledgement.
- An agent first encountered as `idle` establishes a baseline, not an unread entry. Existing `done` agents can seed the queue.
- `blocked` becomes unread on a status hook or a newly observed transition; existing blocked agents are not added blindly at startup.
- A pane-focus event, successful unread jump, or invoking `Alt-u` from a pane marks its Beacon entry read.
- Closed, exited, running, unknown, and missing agents are removed automatically.
- An empty queue requests a soundless notification. Herdr may suppress it without making the shortcut fail.
- Working-agent navigation is live and does not add to or consume the unread queue.
- From outside the working set, `Alt-o` starts with the newest turn; repeated presses advance and wrap.

## Install

Requires macOS, Herdr 0.7.5 or newer, and a Rust toolchain.

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
description = "Jump to newest unread agent"

[[keys.command]]
key = "alt+o"
type = "plugin_action"
command = "shadowfax.beacon.jump-working"
description = "Cycle through working agents"
```

It preserves unrelated configuration, is byte-for-byte idempotent, and refuses to replace any built-in or custom binding already using `Alt-u` or `Alt-o`.

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

Working-agent jumps read the same live agent list but never touch persisted queue state. Herdr's `state_change_seq` supplies the recency order. The action's `HERDR_PLUGIN_CONTEXT_JSON.focused_pane_id` (or `HERDR_PANE_ID`) supplies the cycle cursor, rather than another client's server focus. Standalone commands without pane context fall back to API focus.

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

## Remove

Delete Beacon's `[[keys.command]]` blocks, then run:

```sh
herdr server reload-config
herdr plugin uninstall shadowfax.beacon
```

Use `herdr plugin unlink shadowfax.beacon` instead when Beacon was linked from a local checkout.

## License

[MIT](LICENSE)
