"""Opt-in regression: Alt-' walks the rows the sidebar shows, top to bottom.

Usage: python3 tests/real_sidebar_walk.py AGENTS_BIN BEACON_BIN

Real Herdr with a TUI client in a PTY, a real Agents daemon (folder tree with
its RECENT section) and the Beacon plugin linked with its real keybindings, in
an isolated session that never contacts the live one. Herdr recency is made to
disagree with the sidebar: quiet agents report their idle state after the fresh
ones finish, so their transitions are newer. The test reads the agent order off
the rendered screen, requires Agents' published order to match it, then presses
Alt-' and requires every focus to follow the screen rows and then wrap.
"""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

from vt_screen import Screen

agents_bin = str(Path(sys.argv[1]).resolve())
beacon_bin = str(Path(sys.argv[2]).resolve())
herdr = shutil.which("herdr") or sys.exit("Herdr must be installed")
session = f"bsw-{os.getpid()}"
ROWS, COLS = 50, 200
TITLE = re.compile(r"WALK-[A-E]")

# macOS limits Unix socket paths to 104 bytes; keep every root short.
with tempfile.TemporaryDirectory(prefix="bsw-", dir="/tmp") as temp:
    root = Path(temp).resolve()
    projects = root / "projects"
    for folder in ("alpha", "beta"):
        (projects / folder).mkdir(parents=True)
    config = root / "herdr.toml"
    # Pane borders could show terminal titles; the sidebar must be the only place.
    config.write_text('onboarding = false\n[terminal]\ndefault_shell = "/bin/sh"\n[ui]\npane_borders = "off"\n')
    catalog = root / "catalog.toml"
    catalog.write_text("schema_version = 1\n")
    (root / "home").mkdir()
    plugin_config, plugin_state = root / "agents-config", root / "agents"
    env = {k: v for k, v in os.environ.items() if not k.startswith("HERDR_")}
    # Beacon's shortcut runs as a Herdr action and inherits the server's
    # environment, so it finds this run's Agents endpoint the same way.
    env.update(HOME=str(root / "home"), XDG_CONFIG_HOME=str(root / "config"),
               XDG_STATE_HOME=str(root / "state"), XDG_DATA_HOME=str(root / "data"),
               HERDR_CONFIG_PATH=str(config), TERM="xterm-256color",
               HERDR_AGENTS_STATE=str(plugin_state),
               HERDR_AGENT_DETECTION_MANIFEST_CATALOG_URL=catalog.as_uri())

    def cli(*args):
        result = subprocess.run([herdr, "--session", session, *args], env=env,
                                capture_output=True, text=True, timeout=12)
        assert result.returncode == 0, (args, result.stderr, result.stdout)
        return json.loads(result.stdout) if result.stdout.strip().startswith("{") else result.stdout

    pid, fd = pty.fork()
    if pid == 0:
        os.execve(herdr, [herdr, "--session", session], env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    screen = Screen(ROWS, COLS)

    def pump(seconds=0.1):
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
            screen.feed(chunk)

    daemon = None
    try:
        pump(2)
        sock = cli("status", "--json")["server"]["socket"]
        assert Path(sock).is_relative_to(root), "refuse to contact a non-test socket"
        plugin_config.mkdir()
        (plugin_config / "config.toml").write_text(
            'variant = "text"\nfollow_appearance = false\nidle_grace_seconds = 0\n'
            '[grouping]\nmode = "folders"\nmin_sessions = 1\nroots = [' + json.dumps(str(projects)) + ']\n')
        plugin_env = dict(env, HERDR_SOCKET_PATH=sock, HERDR_PLUGIN_CONFIG_DIR=str(plugin_config),
                          HERDR_BIN_PATH=herdr)
        # Agents metadata needs a registered plugin source in the real server.
        registered = root / "registered"
        registered.mkdir()
        (registered / "herdr-plugin.toml").write_text(
            'id = "shadowfax.agents"\nname = "Agents test"\nversion = "0.1.0"\n'
            'description = "Isolated sidebar walk"\nmin_herdr_version = "0.9.0"\n'
            'platforms = ["macos", "linux"]\n')
        cli("plugin", "link", str(registered))
        cli("plugin", "link", str(Path(beacon_bin).parents[2]))

        def report(pane, state):
            cli("pane", "report-agent", pane, "--source", "beacon:walk", "--agent", "codex", "--state", state)

        def agents():
            return cli("agent", "list")["result"]["agents"]

        def tokens(pane):
            return next(a for a in agents() if a["pane_id"] == pane).get("tokens", {})

        def await_mark(pane, state):
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                if tokens(pane).get("state_" + state):
                    return
                pump(0.1)
            raise AssertionError(f"Agents never showed {pane} as {state}")

        origin = cli("pane", "list")["result"]["panes"][0]["pane_id"]
        split = lambda pane, folder: cli("pane", "split", pane, "--direction", "right", "--cwd",
                                         str(projects / folder), "--no-focus")["result"]["pane"]["pane_id"]
        other = cli("workspace", "create", "--cwd", str(projects / "beta"), "--label", "second",
                    "--no-focus")["result"]["root_pane"]["pane_id"]
        fresh_a, quiet_a, quiet_b = split(origin, "alpha"), split(origin, "alpha"), split(origin, "beta")
        fresh_b, quiet_c = split(other, "beta"), split(other, "alpha")
        names = {fresh_a: "WALK-A", fresh_b: "WALK-B", quiet_a: "WALK-C", quiet_b: "WALK-D", quiet_c: "WALK-E"}
        for pane, name in names.items():
            # Title the terminal, then clear the echoed command from the pane.
            cli("pane", "run", pane, f"printf '\\033]2;{name}\\007'; clear")
        subprocess.run([agents_bin, "configure"], env=plugin_env, check=True, capture_output=True, timeout=25)
        result = subprocess.run([beacon_bin, "install-keybindings"], env=plugin_env,
                                capture_output=True, text=True, timeout=25)
        assert result.returncode == 0, result.stderr
        assert cli("config", "check").strip() == "config: ok"
        daemon = subprocess.Popen([agents_bin, "daemon"], env=plugin_env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        deadline = time.monotonic() + 5
        while not (plugin_state / "control.sock").exists():
            assert time.monotonic() < deadline, "Agents endpoint not ready"
            pump()
        # Two fresh completions lead RECENT; the quiet agents report idle last,
        # so Herdr's recency would put them first.
        for pane in (fresh_a, fresh_b):
            report(pane, "working")
            await_mark(pane, "working")
            report(pane, "idle")
            await_mark(pane, "done")
        for pane in (quiet_a, quiet_b, quiet_c):
            report(pane, "idle")
            await_mark(pane, "idle")
        seq = {a["pane_id"]: a["state_change_seq"] for a in agents()}
        assert max(seq[p] for p in (quiet_a, quiet_b, quiet_c)) > seq[fresh_b], "Herdr recency must disagree"

        by_title = {name: pane for pane, name in names.items()}

        def displayed():
            # The agents panel is the sidebar section below its header; each
            # row carries one title, and pane contents carry none.
            lines = screen.lines()
            start = next((i for i, line in enumerate(lines) if line.lstrip().startswith("agents")), len(lines))
            found = [TITLE.search(line) for line in lines[start + 1:]]
            return [by_title[match.group(0)] for match in found if match]

        def published():
            reply = json.loads(subprocess.run(
                [agents_bin, "workspace-policy"], env=plugin_env, capture_output=True,
                text=True, timeout=10, check=True).stdout)
            return [row["pane_id"] for row in reply["order"] or []]

        deadline = time.monotonic() + 10
        while not (len(displayed()) == len(names) and displayed() == published()):
            assert time.monotonic() < deadline, ("published order differs from the screen",
                                                 displayed(), published(), "\n".join(screen.lines()))
            pump(0.3)
        order = displayed()
        assert order[:2] == [fresh_b, fresh_a], ("RECENT must lead, newest first", order)
        # Each screen row also holds pane content right of the sidebar.
        assert any(re.match(r"\s*RECENT\b", line) for line in screen.lines()), "RECENT header not rendered"

        def focused():
            return next((a["pane_id"] for a in agents() if a["focused"]), None)

        visited = []
        for _ in range(len(order) + 1):
            before = focused()
            os.write(fd, b"\x1b'")  # The installed Alt-' shortcut.
            deadline = time.monotonic() + 5
            while focused() == before:
                assert time.monotonic() < deadline, ("Alt-' did not move focus", visited)
                pump(0.1)
            visited.append(focused())
        rows = [order.index(pane) for pane in visited]
        print(json.dumps({"screen_rows": [names[p] for p in order], "visited_rows": rows}))
        assert visited == order + order[:1], ("Alt-' must walk the sidebar top to bottom, then wrap", rows)
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
