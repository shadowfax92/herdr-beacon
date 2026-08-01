<div align="center">

# 🔦 Beacon

**Jump to the newest unread agent in Herdr.**

[![Herdr 0.7.5+](https://img.shields.io/badge/Herdr-0.7.5%2B-6c71c4)](https://herdr.dev)
[![Rust](https://img.shields.io/badge/built%20with-Rust-b7410e)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

</div>

Beacon remembers background agents that finish or need input. Press `Alt-u` to focus the newest unread agent; press it again for the next one.

- `done` is unread because Herdr uses that status for unseen background completion.
- `blocked` becomes unread when Beacon observes the agent waiting in the background.
- Focusing the pane marks its Beacon entry read.
- Closed, exited, running, idle, and missing agents are removed automatically.
- An empty queue shows `No unread agents` with `--sound none`.

## Install

Requires macOS, Herdr 0.7.5 or newer, and a Rust toolchain.

```sh
herdr plugin install shadowfax92/herdr-beacon --yes
herdr plugin action invoke shadowfax.beacon.install-keybindings
```

Herdr builds and enables Beacon from its locked Rust manifest. The second command adds this conflict-checked block to `~/.config/herdr/config.toml`, backs up an existing config, and reloads Herdr:

```toml
[[keys.command]]
key = "alt+u"
type = "plugin_action"
command = "shadowfax.beacon.jump-unread"
description = "Jump to newest unread agent"
```

It preserves unrelated configuration, is byte-for-byte idempotent, and refuses to replace any built-in or custom binding already using `Alt-u`.

To work on a local checkout instead:

```sh
cargo build --release --locked
herdr plugin link .
herdr plugin action invoke shadowfax.beacon.install-keybindings
```

## How it works

Herdr runs Beacon on agent-status, pane-focus, close, exit, detection, and move events. Beacon validates each event against the live `herdr agent` record, then stores only pane identity, attention status, and ordering metadata in its private plugin state directory.

Herdr hook processes can finish out of order. Beacon orders entries with Herdr's `state_change_seq` and keeps per-pane clear watermarks so a late completion hook cannot resurrect work that was already focused. State updates use a filesystem lock and atomic private files.

Before every jump, Beacon reconciles the queue with `herdr agent list`. This recovers missed `done` agents, prunes stale entries, and rebases ordering after a Herdr server restart. Beacon does not reconstruct missing `blocked` entries because Herdr cannot distinguish a newly blocked agent from one the user already visited.

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

Delete Beacon's `[[keys.command]]` block, then run:

```sh
herdr server reload-config
herdr plugin uninstall shadowfax.beacon
```

Use `herdr plugin unlink shadowfax.beacon` instead when Beacon was linked from a local checkout.

## License

[MIT](LICENSE)
