"""Opt-in regression: verify Beacon's jump changes a real Herdr TUI's contents.

Usage: python3 tests/real_tui_navigation.py BEACON_BIN jump-working|jump-unread tab|workspace
Requires an installed Herdr. Each run owns an isolated PTY, server, and temporary
config/state roots; it never connects to the user's live session.
"""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import time

beacon = str(Path(sys.argv[1]).resolve())
action, destination = sys.argv[2:4]
assert action in ("jump-working", "jump-unread")
assert destination in ("tab", "workspace")
herdr = shutil.which("herdr")
assert herdr, "Herdr must be installed"
session = f"beacon-repro-{os.getpid()}"

with tempfile.TemporaryDirectory(prefix="bcn-nav-", dir="/tmp") as root:
    env = {k: v for k, v in os.environ.items() if not k.startswith("HERDR_")}
    env.update(XDG_CONFIG_HOME=root + "/config", XDG_STATE_HOME=root + "/state",
               HERDR_CONFIG_PATH=str(Path(__file__).with_name("herdr-repro.toml").resolve()),
               TERM="xterm-256color")

    def cli(*args):
        result = subprocess.run([herdr, "--session", session, *args], env=env,
                                capture_output=True, text=True, timeout=8)
        assert result.returncode == 0, result.stderr or result.stdout
        return json.loads(result.stdout) if result.stdout.lstrip().startswith("{") else result.stdout

    pid, fd = pty.fork()
    if pid == 0:
        os.execve(herdr, [herdr, "--session", session], env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))

    def frame(seconds=0.8):
        deadline = time.monotonic() + seconds
        output = b""
        while time.monotonic() < deadline:
            ready, _, _ = select.select([fd], [], [], max(0, deadline - time.monotonic()))
            if ready:
                try:
                    chunk = os.read(fd, 262144)
                except OSError:
                    break
                if not chunk:
                    break
                output += chunk
                if b"\x1b[6n" in chunk:
                    os.write(fd, b"\x1b[1;1R")
        return re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", output).decode("utf-8", errors="replace")

    try:
        frame(2)
        origin = cli("pane", "list")["result"]["panes"][0]["pane_id"]
        created = cli(destination, "create", "--label", "target", "--no-focus")["result"]
        pane = created["root_pane"]["pane_id"]
        socket = cli("status", "--json")["server"]["socket"]
        assert socket.startswith(root + "/"), "refuse to contact a non-test socket"
        plugin_env = dict(env, HERDR_BIN_PATH=herdr, HERDR_SOCKET_PATH=socket,
                          HERDR_PLUGIN_STATE_DIR=root + "/beacon", HERDR_PANE_ID=origin,
                          HERDR_PLUGIN_CONTEXT_JSON=json.dumps({"focused_pane_id": origin}))

        def invoke(command):
            result = subprocess.run([beacon, command], env=plugin_env,
                                    capture_output=True, text=True, timeout=8)
            assert result.returncode == 0, result.stderr or result.stdout

        cli("pane", "run", pane, "printf 'BEACON_CROSS_TAB_VISIBLE\\n'")
        cli("pane", "report-agent", pane, "--agent", "codex", "--state", "working", "--source", "beacon:repro")
        if action == "jump-unread":
            # Seed a real working observation, then finish it while hidden. No
            # production hooks or state are used to bootstrap this test queue.
            plugin_env.update(HERDR_PLUGIN_EVENT="pane.agent_status_changed",
                              HERDR_PLUGIN_EVENT_JSON=json.dumps({"event": "pane_agent_status_changed",
                                  "data": {"type": "pane_agent_status_changed", "pane_id": pane}}))
            invoke("event")
            cli("pane", "report-agent", pane, "--agent", "codex", "--state", "idle", "--source", "beacon:repro")
        assert "BEACON_CROSS_TAB_VISIBLE" not in frame(), "target was visible before navigation"
        invoke(action)
        visible = "BEACON_CROSS_TAB_VISIBLE" in frame()
        print(json.dumps({"action": action, "destination": destination, "target_visible": visible}))
        assert visible, "Beacon succeeded but attached TUI did not display the target tab"
    finally:
        # Never stop the user's server: this environment selects only our named
        # test session. Close the owned PTY even if server shutdown fails.
        try:
            subprocess.run([herdr, "--session", session, "server", "stop"], env=env,
                           capture_output=True, timeout=8)
        finally:
            os.close(fd)
            os.waitpid(pid, 0)
