//! Isolated package-manager fixture. These are real Node installations and filesystem writes,
//! but the npm registry/installer is simulated; no provider, private service or network is used.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use steve_runtime::testkit::{eventually, on_timeout};

use steve_acp::{AcpConfig, managed};
use steve_node_api::*;

use super::{LocalNode, NodeConfig, PolicyCeiling, Storage};

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub home: PathBuf,
    pub cfg: NodeConfig,
}

pub fn executable(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Fixture {
    pub fn new(id: &str, sdk: &str, acp: &str, latest: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let node = dir.path().join("node");
        executable(&node, "#!/bin/sh\necho v26.0.0\n");
        let base = serde_json::to_string(&dir.path()).unwrap();
        let mut entries: BTreeMap<_, _> = steve_harness_spec::components("pi")
            .iter()
            .map(|c| (c.package, c.entry))
            .collect();
        entries.insert(
            "@anthropic-ai/claude-code",
            "@anthropic-ai/claude-code/cli.js",
        );
        entries.insert(
            "@agentclientprotocol/claude-agent-acp",
            "@agentclientprotocol/claude-agent-acp/dist/index.js",
        );
        let entries = serde_json::to_string(&entries).unwrap();
        executable(
            &dir.path().join("npm"),
            &format!(
                r#"#!/usr/bin/env python3
import json, os, pathlib, sys, time
base = pathlib.Path({base})
entries = {entries}
args = sys.argv[1:]
with (base / 'npm.log').open('a') as f: f.write(json.dumps(args) + '\n')
if args[0] == 'view':
    if (base / 'registry-fail').exists(): sys.exit(19)
    print(json.dumps((base / 'latest').read_text().strip()))
    sys.exit(0)
(base / 'started').touch()
until = time.monotonic() + 20
while (base / 'hold').exists() and time.monotonic() < until: time.sleep(.01)
if (base / 'fail').exists():
    print('fixture-secret-must-not-reach-receipt', file=sys.stderr)
    sys.exit(17)
prefix = pathlib.Path(args[args.index('--prefix') + 1]) / 'node_modules'
for spec in args:
    if not spec.startswith('@') or '@' not in spec[1:]: continue
    package, version = spec.rsplit('@', 1)
    if version == 'latest': version = (base / 'latest').read_text().strip()
    if (base / 'wrong-version').exists(): version = '0.0.1'
    p = prefix / package
    p.mkdir(parents=True, exist_ok=True)
    (p / 'package.json').write_text(json.dumps({{'name':package,'version':version}}))
    entry = prefix / entries[package]
    entry.parent.mkdir(parents=True, exist_ok=True)
    entry.write_text('// simulated package entry\n')
"#
            ),
        );
        std::fs::write(dir.path().join("latest"), latest).unwrap();
        for (i, c) in steve_harness_spec::components("pi").iter().enumerate() {
            let root = managed::root(&home);
            let package = root.join(c.package);
            std::fs::create_dir_all(&package).unwrap();
            std::fs::write(
                package.join("package.json"),
                serde_json::json!({"name":c.package,"version":if i==0 {sdk} else {acp}})
                    .to_string(),
            )
            .unwrap();
            let entry = root.join(c.entry);
            std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
            std::fs::write(entry, "// simulated package entry\n").unwrap();
        }
        let cfg = NodeConfig {
            node: NodeId::new(id).unwrap(),
            home: home.join("node"),
            acp: AcpConfig {
                adapters: HashMap::from([("pi".into(), managed::command(&home, "pi").unwrap())]),
                declared: HashMap::new(),
                node,
                claude_executable: PathBuf::new(),
                steve_home: home.clone(),
                codex_home: dir.path().join("codex"),
                user_home: dir.path().to_path_buf(),
                version: "fixture".into(),
            },
            roots: vec![],
            capacity: Capacity {
                sessions: 4,
                jobs: 4,
                execs: 4,
            },
            storage: Storage::default(),
            ceiling: PolicyCeiling::unlimited(),
        };
        Self { dir, home, cfg }
    }
    pub fn node(&self) -> LocalNode {
        LocalNode::open(NodeConfig {
            node: self.cfg.node.clone(),
            home: self.cfg.home.clone(),
            acp: self.cfg.acp.clone(),
            roots: self.cfg.roots.clone(),
            capacity: self.cfg.capacity.clone(),
            storage: self.cfg.storage.clone(),
            ceiling: self.cfg.ceiling.clone(),
        })
        .unwrap()
    }
    pub fn mark(&self, name: &str) {
        std::fs::write(self.dir.path().join(name), "").unwrap();
    }
    pub fn unmark(&self, name: &str) {
        std::fs::remove_file(self.dir.path().join(name)).unwrap();
    }
    pub fn installs(&self) -> usize {
        std::fs::read_to_string(self.dir.path().join("npm.log"))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("\"install\""))
            .count()
    }
    pub fn command(&self, op: &str, target: &str) -> Command<UpgradeHarness> {
        Command {
            id: OperationId::new(op).unwrap(),
            issued_at: Timestamp(1),
            accept_before: Timestamp(i64::MAX),
            grant: GrantId::new("owner").unwrap(),
            body: UpgradeHarness {
                harness: "pi".into(),
                versions: steve_harness_spec::components("pi")
                    .iter()
                    .map(|c| (c.id.into(), target.into()))
                    .collect(),
                self_update: vec![],
            },
        }
    }
}

pub async fn checked(n: &dyn Node) -> HarnessUpdateReport {
    n.harness_updates(true).await.unwrap();
    let checked = std::sync::Mutex::new(None);
    eventually(
        on_timeout(|| async {
            let report = n.harness_updates(false).await;
            format!("component check finished: {report:?}")
        }),
        || async {
            let report = n.harness_updates(false).await.unwrap();
            if !report.checking {
                *checked.lock().unwrap() = Some(report);
                return true;
            }
            false
        },
    )
    .await;
    checked
        .into_inner()
        .unwrap()
        .expect("component check snapshot")
}

pub async fn finished(n: &dyn Node, op: OperationId) -> Receipt {
    let receipt = std::sync::Mutex::new(None);
    eventually(
        on_timeout(|| async {
            let state = n.operation(op.clone()).await;
            format!("installer finished: op={op}, receipt={state:?}")
        }),
        || async {
            let r = n.operation(op.clone()).await.unwrap();
            if r.state.is_finished() {
                *receipt.lock().unwrap() = Some(r);
                return true;
            }
            false
        },
    )
    .await;
    receipt
        .into_inner()
        .unwrap()
        .expect("installer receipt snapshot")
}
