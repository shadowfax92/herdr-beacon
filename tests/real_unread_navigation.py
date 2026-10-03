"""Opt-in regression: Alt-u reaches completions that Herdr acknowledges per tab.

Usage: python3 tests/real_unread_navigation.py AGENTS_BIN BEACON_BIN [SCENARIO...]
Scenarios (default: all): zoomed-peer, sibling, focus-regain, background-tab.

Herdr marks every pane in the active tab read: when an agent finishes there,
when agent focus enters the tab, and when the terminal regains focus. Agents
keeps a per-pane ✓ • mark until that pane itself is focused. Each scenario ends a
turn in a real Herdr session, waits until the real Agents daemon marks it, then
requires Beacon's jump-unread to land on it. Every run owns an isolated PTY,
server, config and state roots; it never contacts the live session.
"""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

agents_bin = str(Path(sys.argv[1]).resolve())
beacon_bin = str(Path(sys.argv[2]).resolve())
SCENARIOS = ("zoomed-peer", "sibling", "focus-regain", "background-tab")
selected = sys.argv[3:] or list(SCENARIOS)
assert set(selected) <= set(SCENARIOS), selected
herdr = shutil.which("herdr") or sys.exit("Herdr must be installed")


def drain(fd, seconds=0.1):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        ready, _, _ = select.select([fd], [], [], max(0, deadline - time.monotonic()))
        if not ready:
            continue
        try:
            chunk = os.read(fd, 262144)
        except OSError:
            return
        if not chunk:
            return
        if b"\x1b[6n" in chunk:
            os.write(fd, b"\x1b[1;1R")


def run(scenario):
    session = f"bun-{os.getpid()}-{SCENARIOS.index(scenario)}"
    # macOS limits Unix socket paths to 104 bytes; keep every root short.
    with tempfile.TemporaryDirectory(prefix="bun-", dir="/tmp") as temp:
        root = Path(temp).resolve()
        config = root / "herdr.toml"
        config.write_text('onboarding = false\n[terminal]\ndefault_shell = "/bin/sh"\n[ui]\n')
        catalog = root / "catalog.toml"
        catalog.write_text("schema_version = 1\n")
        (root / "home").mkdir()
        env = {k: v for k, v in os.environ.items() if not k.startswith("HERDR_")}
        env.update(HOME=str(root / "home"), XDG_CONFIG_HOME=str(root / "config"),
                   XDG_STATE_HOME=str(root / "state"), XDG_DATA_HOME=str(root / "data"),
                   HERDR_CONFIG_PATH=str(config), TERM="xterm-256color",
                   HERDR_AGENT_DETECTION_MANIFEST_CATALOG_URL=catalog.as_uri())

        def cli(*args):
            result = subprocess.run([herdr, "--session", session, *args], env=env,
                                    capture_output=True, text=True, timeout=12)
            assert result.returncode == 0, (args, result.stderr, result.stdout)
            return json.loads(result.stdout) if result.stdout.strip().startswith("{") else result.stdout

        pid, fd = pty.fork()
        if pid == 0:
            os.execve(herdr, [herdr, "--session", session], env)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 50, 200, 0, 0))
        daemon = None
        try:
            drain(fd, 2)
            sock = cli("status", "--json")["server"]["socket"]
            assert Path(sock).is_relative_to(root), "refuse to contact a non-test socket"
            plugin_config, plugin_state = root / "agents-config", root / "agents"
            plugin_config.mkdir()
            (plugin_config / "config.toml").write_text('variant = "text"\nfollow_appearance = false\n')
            plugin_env = dict(env, HERDR_SOCKET_PATH=sock, HERDR_PLUGIN_CONFIG_DIR=str(plugin_config),
                              HERDR_AGENTS_STATE=str(plugin_state), HERDR_BIN_PATH=herdr)
            # Agents metadata needs a registered plugin source in the real server.
            registered = root / "registered"
            registered.mkdir()
            (registered / "herdr-plugin.toml").write_text(
                'id = "shadowfax.agents"\nname = "Agents test"\nversion = "0.1.0"\n'
                'description = "Isolated unread regression"\nmin_herdr_version = "0.9.0"\n'
                'platforms = ["macos", "linux"]\n')
            cli("plugin", "link", str(registered))

            def report(pane, state):
                cli("pane", "report-agent", pane, "--source", "beacon:unread", "--agent", "codex",
                    "--state", state)

            def entry(pane):
                return next(a for a in cli("agent", "list")["result"]["agents"] if a["pane_id"] == pane)

            def await_mark(pane, state):
                deadline = time.monotonic() + 8
                while time.monotonic() < deadline:
                    if entry(pane).get("tokens", {}).get("state_" + state):
                        return
                    drain(fd, 0.1)
                raise AssertionError(f"Agents never showed {pane} as {state}")

            def focused():
                return next((a["pane_id"] for a in cli("agent", "list")["result"]["agents"] if a["focused"]), None)

            def alt_u(cursor):
                result = subprocess.run(
                    [beacon_bin, "jump-unread"], capture_output=True, text=True, timeout=8,
                    env=dict(plugin_env, HERDR_PANE_ID=cursor,
                             HERDR_PLUGIN_CONTEXT_JSON=json.dumps({"focused_pane_id": cursor})))
                assert result.returncode == 0, result.stderr
                drain(fd, 0.3)
                return focused()

            origin = cli("pane", "list")["result"]["panes"][0]["pane_id"]
            if scenario in ("zoomed-peer", "focus-regain"):
                target = cli("pane", "split", origin, "--direction", "right", "--no-focus")["result"]["pane"]["pane_id"]
                finishing = [target]
            else:
                target = cli("tab", "create", "--label", "background", "--no-focus")["result"]["root_pane"]["pane_id"]
                finishing = [target]
                if scenario == "sibling":
                    finishing.append(cli("pane", "split", target, "--direction", "right",
                                         "--no-focus")["result"]["pane"]["pane_id"])
            report(origin, "working")
            report(origin, "idle")
            if scenario in ("zoomed-peer", "focus-regain"):
                cli("pane", "zoom", origin, "--on")
            subprocess.run([agents_bin, "configure"], env=plugin_env, check=True,
                           capture_output=True, timeout=25)
            daemon = subprocess.Popen([agents_bin, "daemon"], env=plugin_env,
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            deadline = time.monotonic() + 5
            while not (plugin_state / "control.sock").exists():
                assert time.monotonic() < deadline, "Agents endpoint not ready"
                drain(fd)
            if scenario == "focus-regain":
                os.write(fd, b"\x1b[O")  # The terminal window loses focus.
                drain(fd, 0.3)
            for pane in finishing:
                report(pane, "working")
                await_mark(pane, "working")
                report(pane, "idle")
            for pane in finishing:
                await_mark(pane, "done")
            cursor = origin
            if scenario == "focus-regain":
                os.write(fd, b"\x1b[I")  # Returning to the window reads the tab.
                drain(fd, 0.5)
            if scenario == "sibling":
                newer = finishing[1]
                assert alt_u(origin) == newer, "first press should take the newest completion"
                cursor = newer
            host = entry(target)["agent_status"]
            assert "•" in entry(target)["tokens"].get("logo", ""), "Agents lost the unread mark"
            landed = alt_u(cursor)
            print(json.dumps({"scenario": scenario, "host_status_before_jump": host,
                              "landed_on_target": landed == target}))
            assert landed == target, f"{scenario}: Alt-u did not reach the marked completion"
        finally:
            if daemon is not None:
                daemon.send_signal(signal.SIGTERM)
                try:
                    daemon.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    daemon.kill()
                    daemon.wait()
            # Only this run's named session; the environment holds no live HERDR_* values.
            try:
                subprocess.run([herdr, "--session", session, "server", "stop"], env=env,
                               capture_output=True, timeout=8)
            finally:
                os.close(fd)
                os.waitpid(pid, 0)


for name in selected:
    run(name)
