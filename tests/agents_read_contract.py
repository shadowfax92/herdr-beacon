#!/usr/bin/env python3
"""Run pinned Agents + Beacon binaries through the real read-control seam.

Only the external host is fake. All sockets, config, CLI focus logs, and plugin
state are private; the daemon child is stopped in finally. Visibility is preseeded
fixture data, not a claim about the skeleton's unimplemented SET command.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


class Host:
    """Small host adapter using Agents' newline JSON RPC test setup."""
    def __init__(self, root):
        self.root = root
        self.workspaces = [
            {"workspace_id": "a", "label": "main"},
            {"workspace_id": "d", "label": "ft"},
            {"workspace_id": "f", "label": "FT"},
            {"workspace_id": "m", "label": "my-ft"},
        ]
        self.unavailable = False
        self.done = threading.Event()
        self.listener = socket.socket(socket.AF_UNIX)
        self.listener.bind(str(root / "host.sock"))
        self.listener.listen()
        self.listener.settimeout(0.05)
        self.handlers = []
        self.thread = threading.Thread(target=self.serve)
        self.thread.start()

    def serve(self):
        while not self.done.is_set():
            try:
                stream, _ = self.listener.accept()
            except TimeoutError:
                continue
            handler = threading.Thread(target=self.handle, args=(stream,))
            self.handlers.append(handler)
            handler.start()

    def handle(self, stream):
        with stream:
            stream.settimeout(3)
            try:
                request = json.loads(stream.makefile("rb").readline())
                method = request["method"]
                error = method == "workspace.list" and self.unavailable
                if method == "workspace.list":
                    result = {"workspaces": self.workspaces}
                elif method == "agent.list":
                    result = {"agents": []}
                elif method == "pane.list":
                    result = {"panes": []}
                elif method == "tab.list":
                    result = {"tabs": []}
                else:
                    result = {}
                reply = {"id": request["id"]}
                reply.update({"error": "fixture unavailable"} if error else {"result": result})
                stream.sendall(json.dumps(reply).encode() + b"\n")
                if method == "events.subscribe":
                    self.done.wait()
            except (OSError, ValueError):
                pass

    def close(self):
        self.done.set()
        self.thread.join()
        self.listener.close()
        for handler in self.handlers:
            handler.join()


FAKE_CLI = r'''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
r=Path(os.environ['BEACON_FIXTURE_ROOT']); args=sys.argv[1:]
with (r/'commands.jsonl').open('a') as f: f.write(json.dumps(args)+'\n')
agents=json.loads((r/'live.json').read_text())
if args[:2]==['agent','list']: result={'agents':agents}
elif args[:2] in [['agent','get'],['agent','focus']]:
    matches=[a for a in agents if a['pane_id']==args[2]]
    if not matches:
        print(json.dumps({'error':{'code':'agent_not_found','message':'fixture missing'}}));sys.exit(1)
    result={'agent':dict(matches[0],focused=args[1]=='focus')}
else: result={'shown':True}
print(json.dumps({'result':result}))
'''


def agent(workspace, status, sequence):
    return dict(pane_id=f"p-{workspace}", terminal_id=f"t-{workspace}", workspace_id=workspace,
                tab_id=f"tab-{workspace}", agent_status=status, focused=False, state_change_seq=sequence)


def run(args):
    agents_binary = args.agents.resolve()
    beacon_binary = args.beacon.resolve()
    hashes = {"agents": digest(agents_binary), "beacon": digest(beacon_binary)}
    if args.agents_sha256:
        assert hashes["agents"] == args.agents_sha256, hashes
    evidence = {"binaries": {"agents": str(agents_binary), "beacon": str(beacon_binary)},
                "sha256": hashes, "cases": []}
    # macOS sockaddr_un requires short paths. Never use inherited Herdr roots.
    with tempfile.TemporaryDirectory(prefix="bpa-", dir="/tmp") as temporary:
        root = Path(temporary)
        host = Host(root)
        child = None
        log = (root / "daemon.log").open("w")
        try:
            for name in ["agents", "config", "beacon", "xdg"]:
                (root / name).mkdir()
            config = root / "config/config.toml"
            config.write_text('[workspace_visibility]\nexcluded_labels = ["ft"]\n')
            (root / "herdr.toml").write_text("")
            (root / "herdr").write_text(FAKE_CLI)
            (root / "herdr").chmod(0o755)
            env = os.environ.copy()
            env.update(HERDR_SOCKET_PATH=str(root / "host.sock"),
                       HERDR_AGENTS_STATE=str(root / "agents"),
                       HERDR_PLUGIN_STATE_DIR=str(root / "agents"),
                       HERDR_PLUGIN_CONFIG_DIR=str(root / "config"),
                       HERDR_CONFIG_PATH=str(root / "herdr.toml"),
                       HERDR_BIN_PATH="/usr/bin/false", XDG_STATE_HOME=str(root / "xdg"))
            env.pop("HERDR_PLUGIN_CONTEXT_JSON", None)
            child = subprocess.Popen([str(agents_binary), "daemon"], env=env, stdout=log, stderr=log)
            deadline = time.monotonic() + 5
            while not (root / "agents/control.sock").exists():
                assert child.poll() is None, "Agents daemon exited"
                assert time.monotonic() < deadline, "Agents endpoint not ready"
                time.sleep(0.01)
            beacon_env = env | {"HERDR_PLUGIN_STATE_DIR": str(root / "beacon"),
                                "HERDR_BIN_PATH": str(root / "herdr"),
                                "HERDR_PANE_ID": "outside", "BEACON_FIXTURE_ROOT": str(root)}

            def live(values):
                (root / "live.json").write_text(json.dumps(values))

            def execute(mode, name, expected, success=True, extra=None, fresh=True):
                state = root / "beacon/state.json"
                if fresh and state.exists():
                    state.unlink()  # Only this fixture's ledger, never owner state.
                (root / "commands.jsonl").write_text("")
                result = subprocess.run([str(beacon_binary), mode], env=beacon_env | (extra or {}),
                                        capture_output=True, text=True, timeout=6)
                commands = [json.loads(line) for line in (root / "commands.jsonl").read_text().splitlines()]
                focused = [c[2] for c in commands if c[:2] == ["agent", "focus"]]
                assert (result.returncode == 0) == success, (name, result.stderr)
                assert focused == expected, (name, focused, expected, result.stderr)
                evidence["cases"].append({"case": name, "exit": result.returncode,
                                          "focus": focused, "stderr": result.stderr.strip()})

            modes = [("jump-unread", "done"), ("jump-working", "working"),
                     ("jump-recent", "idle"), ("jump-recent-reverse", "idle")]
            for shown in [False, True]:
                (root / "agents/workspace-visibility.json").write_text(json.dumps({"version": 1, "show_excluded": shown}))
                for mode, status in modes:
                    live([agent("a", status, 8), agent("d", status, 20 if not mode.endswith("reverse") else 1)])
                    execute(mode, f"{mode}-shown-{shown}", ["p-a"])
                    live([agent("d", status, 20)])
                    execute(mode, f"{mode}-excluded-only-shown-{shown}", [])
            for workspace in ["f", "m"]:
                live([agent(workspace, "working", 10)])
                execute("jump-working", f"exact-label-{workspace}", [f"p-{workspace}"])

            live([agent("d", "done", 20)])
            execute("jump-unread", "before-rename", [])
            host.workspaces[1] = {"workspace_id": "d", "label": "renamed"}
            execute("jump-unread", "rename-out-baseline", [], fresh=False)
            live([agent("d", "done", 22)])
            execute("jump-unread", "rename-out-new-completion", ["p-d"], fresh=False)
            config.write_text('[workspace_visibility]\nexcluded_labels = ["renamed"]\n')
            execute("jump-recent", "fresh-config-excludes-renamed", [], fresh=False)
            config.write_text('[workspace_visibility]\nexcluded_labels = []\n')
            execute("jump-unread", "config-removal-baseline", [], fresh=False)
            live([agent("d", "done", 24)])
            execute("jump-unread", "config-removal-next-completion", ["p-d"], fresh=False)

            live([agent("a", "done", 30)])
            execute("jump-unread", "wrong-session", [], success=False,
                    extra={"HERDR_SOCKET_PATH": str(root / "other.sock")})
            execute("jump-unread", "session-recovery-baseline", [], fresh=False)
            host.unavailable = True
            execute("jump-unread", "host-workspaces-unavailable", [], success=False)
            host.unavailable = False
            execute("jump-unread", "host-recovery-baseline", [], fresh=False)
            live([agent("a", "done", 32)])
            execute("jump-unread", "host-recovery-next-completion", ["p-a"], fresh=False)
            execute("jump-working", "missing-agents-endpoint", [], success=False,
                    extra={"HERDR_AGENTS_STATE": str(root / "missing")})
            live([agent("a", "done", 34)])
            execute("jump-unread", "transport-recovery-baseline", [], fresh=False)
            live([agent("a", "done", 36)])
            execute("jump-unread", "transport-recovery-next-completion", ["p-a"], fresh=False)
            evidence["scope"] = "Actual daemon + actual Beacon CLI; private fake host and focus recorder; visibility preseed only; no live session."
        finally:
            if child is not None:
                child.terminate()
                try:
                    child.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
            host.close()
            log.close()
            if args.output:
                args.output.parent.mkdir(parents=True, exist_ok=True)
                args.output.with_suffix(".daemon.log").write_text((root / "daemon.log").read_text())
    if args.output:
        args.output.write_text(json.dumps(evidence, indent=2) + "\n")
    print(json.dumps({"cases_passed": len(evidence["cases"]), "sha256": hashes, "output": str(args.output)}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agents", type=Path, required=True)
    parser.add_argument("--beacon", type=Path, required=True)
    parser.add_argument("--agents-sha256")
    parser.add_argument("--output", type=Path)
    run(parser.parse_args())
