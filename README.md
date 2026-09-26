<div align="center">

# 🔦 Beacon

**Navigate agents by attention, working turns, or recent activity in Herdr.**

[![Herdr 0.7.5+](https://img.shields.io/badge/Herdr-0.7.5%2B-6c71c4)](https://herdr.dev)
[![Rust](https://img.shields.io/badge/built%20with-Rust-b7410e)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

</div>

Beacon remembers background agents that finish and keeps blocked requests within reach. Workspaces excluded in Agents configuration are always omitted from Beacon tracking and every shortcut, even when their sidebar rows are shown.

| Shortcut | Agents included | Order |
| --- | --- | --- |
| `Alt-u` | Unread completions and all blocked agents, including ones already viewed | Most recent activity first |
| `Alt-o` | Working and blocked agents | Most recent activity first |
| `Alt-'` | Idle, done, and unknown agents, including unread completions | Most recent activity first |
| `Shift-Alt-'` | The same agents | One step backward in that order |

Repeated presses advance from the current agent and wrap at the end. `Alt-u` retains your current pane's position after reading its completion, so newer blockers do not get repeated before older requests. `Alt-o` starts with the newest working or blocked turn when you are outside the working/blocked set; `Alt-'` starts with the newest agent when you are outside the eligible set. `Shift-Alt-'` steps toward newer activity and wraps from newest to oldest; when outside the eligible set it starts at the oldest. Forward and reverse undo each other while the live activity order is unchanged. Activity means Herdr's latest lifecycle transition (`state_change_seq`), not how often you focus a pane. Ties use pane ID, and each press refreshes the live order.

- A new completion (`idle` or `done`) becomes unread when its state-change sequence advances beyond Beacon's last observation or acknowledgement.
- An agent first encountered as `idle` establishes a baseline, not an unread entry. Existing `done` agents can seed the queue.
- `blocked` becomes unread on a status hook or a newly observed transition. All live blockers are also eligible for `Alt-u` independently of unread state, including blockers found at startup or already viewed.
- A pane-focus event, successful unread jump, or invoking `Alt-u` from a pane marks its Beacon entry read.
- Closed, exited, running, unknown, and missing agents are removed from the unread queue automatically.
- With no other unread or blocked destination, `Alt-u` requests a soundless notification. Herdr may suppress it without making the shortcut fail.
- All four modes reconcile the shared eligibility and unread ledger before selection. Successful navigation records confirmed focus evidence without enrolling new work.
- `Alt-o` covers working and blocked agents. `Alt-'` and `Shift-Alt-'` skip both states and refresh their membership on every press.

## Install

Requires macOS or Linux (including WSL), Herdr 0.7.5 or newer, a Rust toolchain, and a running compatible `shadowfax.agents` daemon implementing workspace-policy protocol v1. Deploy compatible Agents and Beacon versions together; missing or older Agents pauses Beacon tracking and navigation.

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

Herdr runs Beacon on agent-status, pane-focus, close, exit, detection, move, and workspace create/close/rename events. Every hook and jump reads one bulk Agents policy and the canonical `herdr agent list` under Beacon's filesystem state lock. Policy is applied before observing lifecycle changes or selecting destinations, so correctness also survives missing or delayed hooks. Pane aliases merge by terminal identity; event payload addresses cannot override a newer live location.

Agents owns `[workspace_visibility].excluded_labels` and exact, case-sensitive label matching. Beacon does not parse that config, hardcode a workspace label, or use `attention_scope`. A valid empty policy is unrestricted over the returned known workspace IDs. Showing/hiding Agents rows changes neither eligibility nor unread history.

The adapter connects directly to `${HERDR_AGENTS_STATE}/control.sock` when set, otherwise `${XDG_STATE_HOME:-$HOME/.local/state}/herdr/plugins/shadowfax.agents/control.sock`. Caller plugin config/state directories are never used for Agents discovery. `HERDR_SOCKET_PATH` is required and normalized lexically; Agents must echo the same session. One whole-request deadline of two seconds covers connect/write/read, with a 256 KiB reply cap and strict version, field type, sorted-set, and set-inclusion checks. `socket2` supplies bounded Unix connection setup.

Missing/stopped/old Agents, invalid policy, wrong session, or transport failure stops navigation and records recovery-needed evidence. An unknown workspace is unresolved and never enrolled or focused. Exclusion removes all navigable records and pane aliases, retaining only terminal/sequence suppression and any confirmed clearing evidence. Policy removal, rename/move out, or recovery baselines the current completion; a later eligible completion can become unread. Blocked/working agents can rejoin their normal categories immediately. Newly encountered identities across a config-change gap are conservatively baselined.

Herdr hook processes can finish out of order. Beacon orders entries by `state_change_seq` and keeps passive observation watermarks separate from confirmed `cleared_through` evidence. A focus or superseding working/unknown observation can clear pending work; an idle baseline or missing pane address does not prove it was viewed. Unchanged status painting does not override acknowledgements. A lower fresh sequence at the same socket path establishes a new baseline instead of replaying pre-restart history.

State writes use locked, atomic, private files. The explicit **v1 → v2 migration** preserves entries and optional `cleared_through` values, then reconciles affected identities against fresh policy. Ambiguous legacy pending entries remain eligible absent an actual policy transition, recovery, or acknowledgement. V2 persists policy label identity, session, exclusion membership, eligible terminal locations, and minimal suppression/recovery evidence. Unknown fields and incomplete v2 files are errors and are not overwritten.

Before deployment, back up the current `state.json` while Beacon writers are quiescent. **Downgrade requires restoring a compatible pre-upgrade state backup:** older binaries reject v2, and deleting policy fields or editing its version loses the recovery contract. Restoring a backup can replay acknowledgements made after capture, and the older binary no longer enforces workspace exclusions. The mediator owns coordinated installation and rollback; this repository's tests use private state only.

The action's `HERDR_PLUGIN_CONTEXT_JSON.focused_pane_id` (or `HERDR_PANE_ID`) supplies the cycle cursor rather than another client's server focus. Standalone commands without pane context fall back to API focus. Immediately before focus, Beacon performs one fresh identity/workspace lookup and one fresh policy read outside its state lock. A changed/moved/excluded target is refused. Successful jumps use two policy requests regardless of candidate count; no per-pane policy requests or subprocesses are created. Herdr has no conditional focus transaction, so a concurrent change after the final check remains a narrow race.

All four shortcuts focus the agent and then explicitly focus its returned tab. Herdr 0.9's `agent focus` alone can report success without switching the visible TUI tab. The explicit tab focus also affects other TUI clients attached to the same server.

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

The Rust CLI suite uses fake Agents sockets, a private fake Herdr executable, and private ledgers. It covers every mode, show/hide independence, protocol failures, request bounds, topology changes, migration, recovery, and the unread move regressions.

To test an actual built Agents daemon and Beacon CLI against a private fake host (no live focus, UI, or installation):

```sh
python3 tests/agents_read_contract.py --agents /absolute/herdr-agents \
  --beacon target/release/herdr-beacon --agents-sha256 <expected-sha256> \
  --output /absolute/read-contract-evidence.json
```

This read-interface test preseeds only fixture visibility. It does not claim Menu toggle or completed Agents visibility-SET integration; those are separate deployment gates. The historical `real_tui_navigation.py` harness requires an explicitly authorized UI test session and a compatible policy endpoint.

## Remove

Delete Beacon's `[[keys.command]]` blocks, then run:

```sh
herdr server reload-config
herdr plugin uninstall shadowfax.beacon
```

Use `herdr plugin unlink shadowfax.beacon` instead when Beacon was linked from a local checkout.

## License

[MIT](LICENSE)
