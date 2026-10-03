# Ownership and navigation

Herdr owns agent identity, lifecycle, and activity order. Its `done` means idle
and unseen, but Herdr acknowledges per tab: every pane in the active tab is
treated as seen, including peers hidden behind a zoomed pane. Agents tracks
completions per pane until that pane is focused and reports those unread marks
in its v2 policy reply. Unread navigation accepts Herdr `done` or `blocked`, or
an Agents mark for the same terminal while the agent is not working. Individual
TUI badges can differ because clients also track viewed completions.

Agents owns workspace eligibility. Its fresh, session-validated policy excludes
workspaces from every Beacon mode, independently of sidebar visibility.

The navigation module (`jump.rs`) exposes `navigate(host, mode, invoking_pane)`.
It reads policy and agents, validates canonical identities, filters and orders
them, then rechecks the target's identity, policy and mode before focusing. The
invoking pane is the cursor; for unread navigation it remains an ordering anchor
after becoming idle but can never be selected as an unread destination.

`HerdrClient` is the seam for host operations. The production adapter (`herdr.rs`)
uses Herdr's CLI and the Agents policy transport (`eligibility.rs`). Its focus
operation includes tab focus because agent focus alone does not reliably navigate
attached TUI clients. Executable tests use private host and policy adapters.

Beacon has no runtime ledger or lifecycle hook subscriptions. Status and policy
changes take effect on the next invocation, including recovery or newly eligible
workspaces. Legacy ledger files are inert; the hidden `event` command only absorbs
previously queued hooks during upgrades. Keybinding installation is the separate,
explicit configuration-writing operation.
