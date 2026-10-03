<div align="center">

# 🔦 Beacon

**Navigate agents by attention, working turns, or recent activity in Herdr.**

[![Herdr 0.7.5+](https://img.shields.io/badge/Herdr-0.7.5%2B-6c71c4)](https://herdr.dev)
[![Rust](https://img.shields.io/badge/built%20with-Rust-b7410e)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

</div>

Beacon queries Herdr and Agents when you press a shortcut. Herdr owns agent status; unread completions come from Herdr's `done` status and the Agents sidebar's ✓ • marks; Beacon handles navigation. Workspaces excluded in Agents configuration are always omitted from every shortcut, even when their sidebar rows are shown.

| Shortcut | Agents included | Order |
| --- | --- | --- |
| `Alt-u` | API `done` and `blocked` agents (including already-viewed blockers), plus completions the Agents sidebar still marks unread | Most recent activity first |
| `Alt-o` | Working and blocked agents | Most recent activity first |
| `Alt-'` | Idle, done, and unknown agents, including unread completions | Most recent activity first |
| `Shift-Alt-'` | The same agents | One step backward in that order |

Repeated presses advance from the current agent and wrap at the end. `Alt-u` retains your current pane's position after reading its completion, so newer blockers do not get repeated before older requests. `Alt-o` starts with the newest working or blocked turn when you are outside the working/blocked set; `Alt-'` starts with the newest agent when you are outside the eligible set. `Shift-Alt-'` steps toward newer activity and wraps from newest to oldest; when outside the eligible set it starts at the oldest. Forward and reverse undo each other while the live activity order is unchanged. Activity means Herdr's latest lifecycle transition (`state_change_seq`), not how often you focus a pane. Ties use pane ID, and each press refreshes the live order.

- `Alt-u` includes agents Herdr currently reports as `done` or `blocked`, and any agent at rest whose completion Agents still marks unread (✓ •) for the same terminal. An unmarked `idle` agent is never an unread destination, regardless of age or earlier status.
- Herdr acknowledges completions per tab, Agents per pane. Focusing a pane clears its mark; Beacon reads both on the next press and does not remember its own read/unread state.
- With no other unread or blocked destination, `Alt-u` requests a soundless notification. Expected notification suppression still counts as success.
- `Alt-o` covers working and blocked agents. `Alt-'` and `Shift-Alt-'` skip both states and refresh membership on every press.

## Install

Requires macOS or Linux (including WSL), Herdr 0.7.5 or newer, a Rust toolchain, and a running compatible `shadowfax.agents` daemon implementing workspace-policy protocol v2. Deploy compatible Agents and Beacon versions together; missing or older Agents stops navigation.

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
description = "Cycle through working or blocked agents"

[[keys.command]]
key = "alt+quote"
type = "plugin_action"
command = "shadowfax.beacon.jump-recent"
description = "Cycle through idle, done, or unknown agents by recent activity"

[[keys.command]]
key = "alt+shift+quote"
type = "plugin_action"
command = "shadowfax.beacon.jump-recent-reverse"
description = "Cycle backward through idle, done, or unknown agents by recent activity"

# Legacy terminals report Shift-apostrophe as a double quote.
[[keys.command]]
key = "alt+double_quote"
type = "plugin_action"
command = "shadowfax.beacon.jump-recent-reverse"
description = "Cycle backward through idle, done, or unknown agents by recent activity"
```

It preserves unrelated configuration, is byte-for-byte idempotent, and refuses to replace any built-in or custom binding already using `Alt-u`, `Alt-o`, `Alt-'`, or `Shift-Alt-'`.

To work on a local checkout instead:

```sh
cargo build --release --locked
herdr plugin link .
herdr plugin action invoke shadowfax.beacon.install-keybindings
```

## How it works

Every shortcut reads a fresh bulk Agents workspace policy (protocol v2, which includes the sidebar's unread marks) and `herdr agent list`. Beacon filters by workspace eligibility and the chosen mode, then orders the candidates by host `state_change_seq`. It keeps no runtime ledger, filesystem lock, lifecycle hooks, migration, or recovery history.

Agents owns `[workspace_visibility].excluded_labels` and exact, case-sensitive label matching. Beacon does not parse that configuration or hardcode labels. Unknown or excluded workspaces cannot be destinations, independently of whether their sidebar rows are shown. A valid empty exclusion list allows every known workspace. Removing an exclusion, moving into an eligible workspace, or recovering the policy endpoint takes effect immediately: an existing API `done` agent is eligible without waiting for another completion.

The policy adapter connects to `${HERDR_AGENTS_STATE}/control.sock` when set, otherwise `${XDG_STATE_HOME:-$HOME/.local/state}/herdr/plugins/shadowfax.agents/control.sock`. `HERDR_SOCKET_PATH` is required and normalized lexically; Agents must echo the same session. One two-second deadline covers connect/write/read, with a 256 KiB reply cap and strict protocol validation. Missing/stopped/old Agents, invalid policy, wrong session, or transport failure stops the action without focusing anything.

The action's `HERDR_PLUGIN_CONTEXT_JSON.focused_pane_id` (or `HERDR_PANE_ID`) supplies the cycle cursor. Standalone commands without pane context fall back to server focus. Immediately before focus, Beacon performs one fresh agent lookup and another policy read. It refuses a target whose identity, location, eligibility, or mode membership changed. Successful jumps use two policy requests regardless of candidate count. Herdr has no conditional focus transaction, so a concurrent change after the final check remains possible.

All four shortcuts focus the agent and then explicitly focus its returned tab. Herdr 0.9's `agent focus` alone can report success without switching the visible TUI tab. The explicit tab focus also affects other TUI clients attached to the same server.

**Unread follows the sidebar marks.** Herdr acknowledges completions per tab. It reports a finished agent `idle` instead of `done` when the agent's tab is active, when `agent focus` or `pane focus` enters the tab, when the terminal window regains focus, or when the tab is switched to. Peers hidden behind a zoomed pane are therefore read before anybody looks at them, and jumping to one acknowledges the rest. Agents clears its ✓ • mark only when that specific pane is focused, and reports its marks with pane and terminal IDs. `Alt-u` accepts a mark only for the same terminal and an agent that is not working. Herdr 0.9's individual TUI clients can still display different unread badges because they keep their own viewed-completion state; Beacon does not reconstruct that.

### Upgrading from the ledger version

Reinstall or relink the plugin after building so Herdr refreshes the manifest and removes the old event registrations. The hidden legacy `event` command is a no-op for hooks already queued during the update. Existing `state.json` and `state.lock` files are ignored and left untouched; navigation needs no `HERDR_PLUGIN_STATE_DIR`. No queue reset or state migration is required. Keep a copy of the old binary and manifest if you need a rollback; older versions will resume using their old ledger, which may contain stale history.

## Inspect and troubleshoot

```sh
herdr plugin list
herdr plugin action list --plugin shadowfax.beacon
herdr plugin log list --plugin shadowfax.beacon --limit 20
```

Beacon has no popup or queue panel. It also has no manual mark-unread or defer command.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

The Rust CLI suite uses private Agents sockets and a fake Herdr executable. It covers host-owned acknowledgement, Agents unread marks, all modes, cursor ordering, legacy-state independence, workspace policy, protocol failures, request bounds, and targets changing before focus.

To test an actual built Agents daemon and Beacon CLI against a private fake host (no live focus, UI, or installation):

```sh
python3 tests/agents_read_contract.py --agents /absolute/herdr-agents \
  --beacon target/release/herdr-beacon --agents-sha256 <expected-sha256> \
  --output /absolute/read-contract-evidence.json
```

This read-interface test preseeds only fixture visibility. It does not claim Menu toggle or completed Agents visibility-SET integration; those are separate deployment gates. The opt-in `real_tui_navigation.py` harness uses an isolated PTY/server and private policy endpoint to verify visible tab navigation; it never targets the live session.

The opt-in `real_unread_navigation.py` harness runs real Herdr, a real Agents daemon and Beacon in an isolated session. It checks that `Alt-u` reaches completions Herdr acknowledges per tab: a peer finishing behind a zoomed pane, a sibling acknowledged by the previous jump, and a completion acknowledged when the terminal regains focus.

```sh
python3 tests/real_unread_navigation.py /absolute/herdr-agents target/release/herdr-beacon
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
