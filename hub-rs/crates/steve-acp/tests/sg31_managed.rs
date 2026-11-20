//! CLI execution in a separate test process with an isolated prefix and a finite fake npm.
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use steve_acp::managed;
fn python() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|p| p.join("python3"))
        .find(|p| p.is_file())
        .unwrap()
}
fn write_exe(p: &Path, body: &str) {
    std::fs::write(p, body).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700)).unwrap();
}
#[test]
fn sg31_managed_child() {
    let Some(home) = std::env::var_os("SG31_CLI_CHILD") else {
        return;
    };
    let args: Vec<String> = serde_json::from_str(&std::env::var("SG31_ARGS").unwrap()).unwrap();
    let node = std::env::var_os("SG31_NODE").map(PathBuf::from);
    std::process::exit(managed::cli(Path::new(&home), node.as_deref(), &args));
}
struct Fixture {
    dir: tempfile::TempDir,
    home: PathBuf,
    bin: PathBuf,
    log: PathBuf,
    ids: Vec<&'static str>,
}
impl Fixture {
    fn new(npm: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let bin = dir.path().join("bin");
        let log = dir.path().join("npm.jsonl");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(managed::dir(&home)).unwrap();
        write_exe(&bin.join("node"), "#!/bin/sh\nexit 0\n");
        if npm {
            let script = format!(
                "#!{}\n{}",
                python().display(),
                r#"import json, os, sys
from pathlib import Path
args=sys.argv[1:]
with open(os.environ['SG31_NPM_LOG'],'a') as f: f.write(json.dumps(args)+'\n')
mode=os.environ.get('SG31_NPM_MODE','ok')
if args[0]=='view':
    if mode=='view-fail': print('scripted registry refusal',file=sys.stderr); sys.exit(1)
    print(json.dumps('2.0.0')); sys.exit(0)
if args[0]!='install': sys.exit(2)
prefix=Path(args[args.index('--prefix')+1])
specs=args[args.index('--engine-strict')+1:]
if mode=='fail' or (mode=='fallback' and (len(specs)==int(os.environ['SG31_TOTAL']) or os.environ['SG31_BAD']+'@latest' in specs)):
    print('scripted install refusal',file=sys.stderr); sys.exit(1)
for spec in specs:
    name=spec.rsplit('@',1)[0]
    p=prefix/'node_modules'/name/'package.json'; p.parent.mkdir(parents=True,exist_ok=True);p.write_text(json.dumps({'version':'2.0.0'}))
"#
            );
            write_exe(&bin.join("npm"), &script);
        }
        let ids = steve_harness_spec::builtin_packages()
            .map(|(id, _)| id)
            .collect();
        Self {
            dir,
            home,
            bin,
            log,
            ids,
        }
    }
    fn selection(&self, ids: &[&str]) {
        std::fs::write(
            managed::dir(&self.home).join("selected.json"),
            serde_json::to_vec(ids).unwrap(),
        )
        .unwrap();
    }
    fn install_manifest(&self, id: &str, version: &str) {
        let p = managed::root(&self.home)
            .join(managed::package(id).unwrap())
            .join("package.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, json!({"version":version}).to_string()).unwrap();
    }
    fn run(&self, args: &[&str], mode: &str, lang: &str) -> Output {
        let total: usize = self
            .ids
            .iter()
            .take(2)
            .map(|id| managed::packages(id).len())
            .sum();
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "sg31_managed_child", "--nocapture"])
            .env("SG31_CLI_CHILD", &self.home)
            .env("SG31_ARGS", serde_json::to_string(args).unwrap())
            .env("SG31_NODE", self.bin.join("node"))
            .env("SG31_NPM_LOG", &self.log)
            .env("SG31_NPM_MODE", mode)
            .env("SG31_BAD", managed::package(self.ids[1]).unwrap())
            .env("SG31_TOTAL", total.to_string())
            .env("STEVE_LANG", lang)
            .env("PATH", &self.bin)
            .current_dir(self.dir.path())
            .output()
            .unwrap()
    }
    fn calls(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}
fn json_line(out: &Output) -> Value {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| serde_json::from_str(l).ok())
        .unwrap_or_else(|| {
            panic!(
                "no JSON: {} / {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        })
}
#[test]
fn sg31_managed_status_and_list_keep_json_selection_detection_and_human_states() {
    for lang in ["en", "zh-CN"] {
        let f = Fixture::new(true);
        let first = f.ids[0];
        f.selection(&[first]);
        f.install_manifest(first, "1.0.0");
        let out = f.run(&["status", "--json"], "ok", lang);
        assert!(out.status.success());
        let list = json_line(&out);
        let rows = list.as_array().unwrap();
        let a = rows.iter().find(|v| v["id"] == first).unwrap();
        assert_eq!(a["installed"], "1.0.0");
        assert_eq!(a["latest"], "2.0.0");
        assert_eq!(f.calls().len(), 1);
        let out = f.run(&["list"], "ok", lang);
        let v = json_line(&out);
        assert_eq!(v["npm"], true);
        assert_eq!(v["chosen"], true);
        assert!(
            v["adapters"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["detected"] == false)
        );
        assert_eq!(v["detected_path_only"], json!([]));
        let out = f.run(&["status"], "ok", lang);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains(first) && text.contains("1.0.0") && text.contains("2.0.0"));
    }
    let f = Fixture::new(false);
    f.selection(&[]);
    let out = f.run(&["status", "--json"], "ok", "en");
    assert!(out.status.success());
    assert!(
        json_line(&out)
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["latest"].is_null())
    );
    assert!(f.calls().is_empty());
    let out = f.run(&["status"], "ok", "en");
    assert!(String::from_utf8_lossy(&out.stdout).contains("npm"));
}
#[test]
fn sg31_managed_install_selection_fallback_and_partial_failure_keep_write_order() {
    for case in [
        "append",
        "only",
        "fallback",
        "only-fallback",
        "fail",
        "upgrade",
    ] {
        let f = Fixture::new(true);
        let a = f.ids[0];
        let b = f.ids[1];
        if case == "append" {
            f.selection(&[a]);
        } else {
            f.selection(&[]);
        }
        let args = match case {
            "append" => vec!["install", b],
            "only" => vec!["install", a, "--only"],
            "upgrade" => {
                f.selection(&[a]);
                vec!["upgrade"]
            }
            "only-fallback" => vec!["install", a, b, "--only"],
            _ => vec!["install", a, b],
        };
        let mode = if case.contains("fallback") {
            "fallback"
        } else if case == "fail" {
            "fail"
        } else {
            "ok"
        };
        let out = f.run(&args, mode, "en");
        assert_eq!(
            out.status.code(),
            Some(if case.contains("fallback") || case == "fail" {
                1
            } else {
                0
            }),
            "{case}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let selected = managed::selection(&f.home).unwrap();
        let expected = match case {
            "append" => vec![a, b],
            "only-fallback" => vec![a, b],
            "fail" => vec![],
            _ => vec![a],
        };
        assert_eq!(selected, expected, "{case}");
        let installs: Vec<_> = f
            .calls()
            .into_iter()
            .filter(|v| v[0] == "install")
            .collect();
        assert_eq!(
            installs.len(),
            if case.contains("fallback") || case == "fail" {
                3
            } else {
                1
            },
            "{case}"
        );
        if case.contains("fallback") {
            assert_eq!(
                managed::installed(&f.home, managed::package(a).unwrap()).as_deref(),
                Some("2.0.0")
            );
            assert!(managed::installed(&f.home, managed::package(b).unwrap()).is_none());
        }
    }
}
#[test]
fn sg31_managed_validation_and_empty_only_do_not_invoke_npm_or_rewrite_prior_choice() {
    for case in [
        "unknown",
        "path-only",
        "bad-upgrade",
        "usage",
        "only-empty",
        "missing-npm",
    ] {
        let f = Fixture::new(case != "missing-npm");
        let a = f.ids[0];
        f.selection(&[a]);
        let path_only = steve_harness_spec::builtin_ids()
            .find(|id| !steve_harness_spec::is_npm_managed(id))
            .unwrap();
        let args = match case {
            "unknown" => vec!["install", "future-unlisted", "--only"],
            "path-only" => vec!["install", path_only],
            "bad-upgrade" => vec!["upgrade", a],
            "usage" => vec!["unrecognized"],
            "only-empty" => vec!["install", "--only"],
            _ => vec!["install", a],
        };
        let out = f.run(&args, "ok", "en");
        assert_eq!(
            out.status.code(),
            Some(if case == "only-empty" { 0 } else { 2 }),
            "{case}"
        );
        assert!(f.calls().is_empty(), "{case}");
        assert_eq!(
            managed::selection(&f.home).unwrap(),
            if case == "only-empty" {
                Vec::<&str>::new()
            } else {
                vec![a]
            }
        );
    }
}
