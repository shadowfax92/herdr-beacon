//! Executable-level fixture: the real Beacon binary talks to a private policy
//! socket and a fake Herdr CLI. Focus mutates only the fixture host's status,
//! exercising host-owned acknowledgement across separate Beacon processes.
use super::PolicyServer;
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};
use tempfile::{tempdir, TempDir};

pub struct Fixture {
    pub root: TempDir,
    pub policy: PolicyServer,
}
impl Fixture {
    pub fn new() -> Self {
        let root = tempdir().unwrap();
        let policy = PolicyServer::new(&root.path().join("agents"), &["a", "d"], &["d"]);
        fs::write(
            root.path().join("herdr"),
            r#"#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
r=Path(os.environ['FIXTURE_ROOT']); args=sys.argv[1:]
with (r/'commands').open('a') as f: f.write(' '.join(args)+'\n')
agents=json.loads((r/'agents.json').read_text())
if ' '.join(args[:2]) == os.environ.get('FAKE_FAIL'):
    print(json.dumps({'error':{'code':'fixture_error','message':'injected host failure'}}));sys.exit(1)
if args[:2]==['agent','get'] and (r/'move-on-get.json').exists():
    agents=json.loads((r/'move-on-get.json').read_text())
    (r/'move-on-get.json').unlink();(r/'agents.json').write_text(json.dumps(agents))
if args[:2]==['agent','list']: result={'agents':agents}
elif args[:2]==['agent','get']:
    if (r/'preflight.json').exists(): agents=json.loads((r/'preflight.json').read_text())
    matches=[a for a in agents if a['pane_id']==args[2]]
    if not matches:
        print(json.dumps({'error':{'code':'agent_not_found','message':'missing'}}));sys.exit(1)
    result={'agent':matches[0]}
elif args[:2]==['agent','focus']:
    matches=[a for a in agents if a['pane_id']==args[2]]
    assert matches
    for a in agents: a['focused']=a['pane_id']==args[2]
    if matches[0]['agent_status']=='done': matches[0]['agent_status']='idle'
    (r/'agents.json').write_text(json.dumps(agents))
    result={'agent':matches[0]}
elif args[:2]==['tab','focus']: result={'type':'tab_focus'}
elif args[:2]==['notification','show']:
    reason=os.environ.get('FAKE_NOTIFICATION_REASON','shown')
    result={'shown':reason=='shown','reason':reason}
else: raise AssertionError(args)
print(json.dumps({'result':result}))
"#,
        )
        .unwrap();
        fs::set_permissions(root.path().join("herdr"), fs::Permissions::from_mode(0o755)).unwrap();
        let f = Self { root, policy };
        f.live(vec![]);
        f
    }
    pub fn live(&self, agents: Vec<Value>) {
        fs::write(
            self.root.path().join("agents.json"),
            json!(agents).to_string(),
        )
        .unwrap();
    }
    pub fn command(&self, command: &str) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_herdr-beacon"));
        c.arg(command)
            .env("HERDR_BIN_PATH", self.root.path().join("herdr"))
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .env("HERDR_AGENTS_STATE", self.root.path().join("agents"))
            .env("HERDR_SOCKET_PATH", "/test/./host.sock")
            .env("FIXTURE_ROOT", self.root.path())
            .env("HERDR_PANE_ID", "outside")
            .env_remove("HERDR_PLUGIN_CONTEXT_JSON");
        c
    }
    pub fn run(&self, command: &str) -> Output {
        self.command(command).output().unwrap()
    }
    pub fn ok(&self, c: &str) {
        let o = self.run(c);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    pub fn commands(&self) -> String {
        fs::read_to_string(self.root.path().join("commands")).unwrap_or_default()
    }
    pub fn focuses(&self) -> Vec<String> {
        fs::read_to_string(self.root.path().join("commands"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.strip_prefix("agent focus ").map(str::to_string))
            .collect()
    }
}
pub fn agent(ws: &str, status: &str, seq: u64) -> Value {
    json!({"pane_id":format!("p-{ws}"),"workspace_id":ws,"terminal_id":format!("t-{ws}"),"tab_id":format!("tab-{ws}"),"agent_status":status,"state_change_seq":seq,"focused":false})
}
