use super::fixture::{self, Env};
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use steve_acp::managed;
use steve_node::LocalNode;
use steve_node_api::*;
use steve_runtime::testkit::{eventually, process::Command, quiet_for};

#[test]
fn sg34_adapter_policy_table() {
    if let Ok(case) = std::env::var("SG34_ADAPTER_CHILD") {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(policy_case(&case));
        return;
    }
    let path = std::env::var_os("PATH").unwrap();
    let real_node = steve_acp::which_in("node", Some(&path)).unwrap();
    let python = steve_acp::which_in("python3", Some(&path)).unwrap();
    for case in [
        "auto",
        "check",
        "off",
        "lookup-error",
        "install-error",
        "busy",
    ] {
        let home = tempfile::tempdir().unwrap();
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args([
            "--exact",
            "adapters::sg34_adapter_policy_table",
            "--nocapture",
        ])
        .env("SG34_ADAPTER_CHILD", case)
        .env(
            "STEVE_ADAPTER_UPDATES",
            if matches!(case, "check" | "off") {
                case
            } else {
                "auto"
            },
        )
        .env("SG34_REAL_NODE", &real_node)
        .env("SG34_PYTHON", &python)
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("FAKE_ACP_") {
                c.env_remove(key);
            }
        }
        for id in steve_acp::adapters::builtin_ids() {
            for component in steve_acp::adapters::components(id) {
                if let Some(key) = component.executable_env {
                    c.env_remove(key);
                }
            }
        }
        let out = c.output().unwrap();
        assert!(
            out.status.success(),
            "{case}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
async fn policy_case(case: &str) {
    let e = Env::new();
    let home = e.dir.path().join("steve");
    let log = e.dir.path().join("npm.log");
    let node = e.dir.path().join("bin/node");
    let npm = e.dir.path().join("bin/npm");
    fixture::executable(
        &node,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo v24.0.0; else exec '{}' \"$@\"; fi\n",
            std::env::var("SG34_REAL_NODE").unwrap()
        ),
    );
    let fake = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../steve-acp/tests/fake-acp.js")
        .canonicalize()
        .unwrap();
    let asset = url::Url::from_file_path(fake).unwrap();
    let ids = ["codex", "claude-code"];
    for id in [ids[0]] {
        let command = managed::command(&home, id).unwrap();
        std::fs::create_dir_all(command.parent().unwrap()).unwrap();
        std::fs::write(&command, format!("import '{}';\n", asset)).unwrap();
        for package in managed::packages(id) {
            let package = managed::root(&home).join(package);
            std::fs::create_dir_all(&package).unwrap();
            std::fs::write(
                package.join("package.json"),
                json!({"version":"1.0.0","type":"module"}).to_string(),
            )
            .unwrap();
        }
    }
    std::fs::write(managed::dir(&home).join("selected.json"), r#"["codex"]"#).unwrap();
    fixture::executable(
        &npm,
        &format!(
            r#"#!{}
import json,os,pathlib,sys
args=sys.argv[1:]
with open({},'a') as out: out.write(json.dumps(args)+'\n')
case=os.environ['SG34_ADAPTER_CHILD']
if args[0]=='--version': print('10.0.0')
elif args[0]=='view':
    if case=='lookup-error': sys.exit(7)
    print('"9.9.9"')
elif args[0]=='install':
    if case=='install-error': sys.exit(8)
    root=pathlib.Path(args[args.index('--prefix')+1])/'node_modules'
    for arg in args:
        if arg.endswith('@latest'):
            p=root/arg.rsplit('@',1)[0];p.mkdir(parents=True,exist_ok=True)
            (p/'package.json').write_text(json.dumps({{'version':'9.9.9','type':'module'}}))
"#,
            std::env::var("SG34_PYTHON").unwrap(),
            serde_json::to_string(&log.display().to_string()).unwrap()
        ),
    );
    let mut cfg = e.config(false);
    cfg.acp.node = node;
    cfg.acp.adapters =
        HashMap::from(ids.map(|id| (id.to_string(), managed::command(&home, id).unwrap())));
    let n = LocalNode::open(cfg).unwrap();
    n.connect(e.sink.clone(), None);
    if case == "busy" {
        let w = e.workspace(&n).await;
        n.exec(fixture::now_cmd(
            "sg34-busy",
            fixture::spec(&w, "mkfifo hold; cat hold"),
        ))
        .await
        .unwrap();
    }
    n.probe(false).await.unwrap();
    tokio::task::yield_now().await;
    tokio::time::pause();
    if case == "busy" {
        tokio::time::advance(Duration::from_secs(61)).await;
    } else {
        quiet_for(Duration::from_secs(61)).await;
    }
    tokio::time::resume();
    let seen = std::sync::Mutex::new(None);
    let save = &seen;
    assert!(
        eventually(format!("adapter update {case} observed"), || async {
            let caps = n.probe(false).await.unwrap();
            let update = caps
                .harnesses
                .iter()
                .find(|h| h.name == "codex")
                .unwrap()
                .adapter
                .as_ref()
                .unwrap()
                .update
                .clone()
                .unwrap();
            if case == "off" || update.checked.is_some() {
                *save.lock().unwrap() = Some(caps);
                true
            } else {
                false
            }
        })
        .await
    );
    let caps = seen.into_inner().unwrap().unwrap();
    let u = caps
        .harnesses
        .iter()
        .find(|h| h.name == "codex")
        .unwrap()
        .adapter
        .as_ref()
        .unwrap()
        .update
        .as_ref()
        .unwrap();
    assert_eq!(
        u.policy,
        if matches!(case, "off" | "check") {
            case
        } else {
            "auto"
        }
    );
    assert!(!u.pending || case == "busy");
    match case {
        "off" => {
            assert!(u.checked.is_none());
            assert!(u.latest.is_none());
            assert_eq!(u.installed.as_deref(), Some("1.0.0"));
        }
        "auto" => {
            assert_eq!(u.installed.as_deref(), Some("9.9.9"));
            assert!(u.error.is_none());
        }
        "check" => {
            assert_eq!(u.installed.as_deref(), Some("1.0.0"));
            assert_eq!(u.latest.as_deref(), Some("9.9.9"));
            assert!(u.error.is_none());
        }
        "busy" => {
            assert!(u.pending);
            assert_eq!(u.installed.as_deref(), Some("1.0.0"));
        }
        "lookup-error" => {
            assert_eq!(
                u.error.as_deref(),
                Some(
                    "Cannot query the npm registry; check the Node's network and npm configuration"
                )
            );
            assert!(u.latest.is_none());
        }
        "install-error" => {
            assert!(
                u.error
                    .as_deref()
                    .unwrap()
                    .contains("npm installation failed")
            );
            assert_eq!(u.installed.as_deref(), Some("1.0.0"));
        }
        _ => unreachable!(),
    }
    let left = caps
        .harnesses
        .iter()
        .find(|h| h.name == "claude-code")
        .unwrap()
        .adapter
        .as_ref()
        .unwrap()
        .update
        .as_ref()
        .unwrap();
    assert!(left.checked.is_none());
    assert!(left.latest.is_none());
    let calls = std::fs::read_to_string(log).unwrap_or_default();
    if matches!(case, "off" | "check" | "busy" | "lookup-error") {
        assert!(!calls.contains("\"install\""));
    }
    n.shutdown().await;
}
