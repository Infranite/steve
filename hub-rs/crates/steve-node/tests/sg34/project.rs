use super::fixture;
use std::path::{Path, PathBuf};
use steve_node::project_setup::{self, Branch, Remote, SetupError, Staging};
use steve_runtime::testkit::process::Command;

#[test]
fn sg34_clone_contracts() {
    if let Ok(case) = std::env::var("SG34_CLONE_CHILD") {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(clone_case(&case));
        return;
    }
    for case in [
        "success",
        "late-conflict",
        "broken",
        "detached",
        "auth",
        "network",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join("bin/git");
        let script = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sg34/fake_git.py"),
        )
        .unwrap();
        fixture::executable(&git, &script);
        let old = std::env::var_os("PATH").unwrap();
        let path = std::env::join_paths(
            std::iter::once(dir.path().join("bin")).chain(std::env::split_paths(&old)),
        )
        .unwrap();
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "project::sg34_clone_contracts", "--nocapture"])
            .env("SG34_CLONE_CHILD", case)
            .env("SG34_CLONE_ROOT", dir.path())
            .env("SG34_CLONE_LOG", dir.path().join("git.json"))
            .env("GIT_CONFIG_GLOBAL", dir.path().join("owner-config"))
            .env("GIT_TRACE", "1")
            .env("GIT_CURL_VERBOSE", "1")
            .env("GIT_WORK_TREE", "untrusted-worktree")
            .env("PATH", path)
            .env("HOME", dir.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{case}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!String::from_utf8_lossy(&out.stdout).contains("SG34 raw diagnostic"));
        assert!(!String::from_utf8_lossy(&out.stderr).contains("SG34 raw diagnostic"));
    }
}
async fn clone_case(case: &str) {
    let root = PathBuf::from(std::env::var_os("SG34_CLONE_ROOT").unwrap());
    let target = project_setup::destination(&root, "destination").unwrap();
    let stage = Staging::new(&target).unwrap();
    let remote = Remote::new("https://example.test/o/r.git".into()).unwrap();
    let branch = if case == "broken" {
        None
    } else {
        Some(Branch::new("topic".into()).unwrap())
    };
    let mut progress = vec![];
    let result = stage
        .clone_repo(remote, branch, |phase, pct| progress.push((phase, pct)))
        .await;
    let expected = match case {
        "broken" => Err(SetupError::DefaultBranch),
        "detached" => Err(SetupError::Branch),
        "auth" => Err(SetupError::Authentication),
        "network" => Err(SetupError::Network),
        _ => Ok(()),
    };
    assert_eq!(result, expected, "{case}");
    assert_eq!(
        progress,
        [("receiving", 25), ("resolving", 100), ("checkout", 100)]
    );
    let log: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("git.json")).unwrap()).unwrap();
    let args: Vec<_> = log["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        args.windows(2)
            .any(|a| a == ["-c", "core.hooksPath=/dev/null"])
    );
    assert!(
        args.windows(2)
            .any(|a| a == ["-c", "http.followRedirects=false"])
    );
    assert!(args.contains(&"--no-recurse-submodules"));
    assert!(args.contains(&"--template="));
    assert!(
        args.windows(2)
            .any(|a| a == ["--", "https://example.test/o/r.git"])
    );
    assert_eq!(
        log["env"]["GIT_CONFIG_GLOBAL"],
        root.join("owner-config").display().to_string()
    );
    assert_eq!(log["env"]["GIT_ALLOW_PROTOCOL"], "https:ssh");
    assert_eq!(log["env"]["GIT_CONFIG_PARAMETERS"], "");
    assert_eq!(log["env"]["GIT_CONFIG_COUNT"], "0");
    assert_eq!(log["env"]["GIT_TERMINAL_PROMPT"], "0");
    assert_eq!(log["env"]["SSH_ASKPASS_REQUIRE"], "never");
    assert!(log["env"]["GIT_TRACE"].is_null());
    assert!(log["env"]["GIT_CURL_VERBOSE"].is_null());
    assert_eq!(log["env"]["GIT_TRACE2"], "0");
    assert_eq!(log["env"]["GIT_TRACE2_EVENT"], "0");
    assert!(log["env"]["GIT_WORK_TREE"].is_null());
    assert!(
        log["env"]["GIT_DIR"]
            .as_str()
            .unwrap()
            .ends_with("unused-git-dir")
    );
    if result.is_ok() {
        if case == "late-conflict" {
            std::fs::create_dir(&target).unwrap();
            std::fs::write(target.join("owner"), "retained").unwrap();
            assert_eq!(stage.publish(&target), Err(SetupError::Conflict));
            assert_eq!(
                std::fs::read_to_string(target.join("owner")).unwrap(),
                "retained"
            );
            assert!(!target.join("kept").exists());
        } else {
            stage.publish(&target).unwrap();
            assert_eq!(
                std::fs::read_to_string(target.join("kept")).unwrap(),
                "complete"
            );
            assert_eq!(
                std::fs::read_to_string(target.join(".git/HEAD")).unwrap(),
                "ref: refs/heads/topic\n"
            );
        }
    } else {
        drop(stage);
        assert!(!target.exists());
    }
    assert!(!std::fs::read_dir(&root).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".steve-clone-")
    }));
}
