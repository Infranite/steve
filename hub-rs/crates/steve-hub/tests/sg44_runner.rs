use std::path::{Path, PathBuf};
use steve_hub::upgrades::{
    Operation, Updates,
    runner::{self, Spec},
};
use steve_runtime::testkit::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
fn executable(p: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(p, body).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}
fn complete(dir: &Path, version: &str, commit: &str) {
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::create_dir_all(dir.join("web/dist")).unwrap();
    std::fs::write(dir.join("bin/steve-hub"), "").unwrap();
    std::fs::write(dir.join("web/dist/index.html"), "").unwrap();
    std::fs::write(
        dir.join("release.json"),
        serde_json::to_vec(&serde_json::json!({"version":version,"commit":commit})).unwrap(),
    )
    .unwrap();
}
#[test]
fn sg44_legacy_runner_binds_tags_reuses_complete_builds_and_cleans_only_owned_failures() {
    for mode in [
        "success",
        "fetch-fallback",
        "prebuilt",
        "incomplete",
        "owned-failure",
        "foreign-failure",
        "deploy-refused",
        "tag-mismatch",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("instance");
        let repo = root.join("repo");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::create_dir(repo.join("ops")).unwrap();
        let script = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sg44/legacy_release.py"),
        )
        .unwrap();
        std::fs::write(repo.join("ops/local-release.py"), script).unwrap();
        git(&repo, &["add", "ops"]);
        git(&repo, &["commit", "-qm", "fixture release"]);
        let commit = git(&repo, &["rev-parse", "HEAD"]);
        if mode == "tag-mismatch" {
            git(
                &repo,
                &["commit", "--allow-empty", "-qm", "different release"],
            );
        }
        git(&repo, &["tag", "v1.2.3"]);
        let python = root.join("python3");
        executable(
            &python,
            &format!(
                "#!/bin/sh\nexport SG44_HOME='{}' SG44_MODE='{}'\nexec python3 \"$@\"\n",
                home.display(),
                mode
            ),
        );
        let u = Updates::journal(&home);
        let id = uuid::Uuid::new_v4().to_string();
        let op:Operation=serde_json::from_value(serde_json::json!({
            "id":id,"idempotency_key":"fixture","version":"1.2.3","phase":"staging","created_at":1,"updated_at":1,"requested_by":"fixture",
            "runner":Spec{kind:"local_release".into(),repo:repo.to_string_lossy().into_owned(),
                source:if mode=="fetch-fallback"{root.join("missing").to_string_lossy().into_owned()}else{repo.to_string_lossy().into_owned()},
                commit:Some(commit.clone()),python:python.to_string_lossy().into_owned(),cargo:"unused".into(),..Default::default()}
        })).unwrap();
        u.save(&op).unwrap();
        let candidate = home.join(format!("releases/1.2.3-local-{}", &commit[..7]));
        if mode == "prebuilt" {
            complete(&candidate, "1.2.3", &commit);
        }
        if mode == "incomplete" {
            std::fs::create_dir_all(&candidate).unwrap();
        }
        let rc = runner::main(&home, &id);
        let actual = u.operation(&id).unwrap();
        let failed = [
            "incomplete",
            "owned-failure",
            "foreign-failure",
            "tag-mismatch",
        ]
        .contains(&mode);
        assert_eq!(rc, i32::from(failed), "{mode}: {actual:?}");
        assert_eq!(
            actual.phase,
            if failed || mode == "deploy-refused" {
                "failed"
            } else {
                "succeeded"
            },
            "{mode}"
        );
        let trace = std::fs::read_to_string(home.join("trace")).unwrap_or_default();
        match mode {
            "owned-failure" => {
                assert!(!candidate.exists());
                assert_eq!(trace, "build\n");
            }
            "foreign-failure" => {
                assert!(candidate.join("partial").exists());
                assert_eq!(trace, "build\n");
            }
            "incomplete" | "tag-mismatch" => assert!(trace.is_empty()),
            "prebuilt" => assert_eq!(trace, "deploy\n"),
            _ => assert_eq!(trace, "build\ndeploy\n"),
        }
        if !failed {
            assert_eq!(actual.target.as_deref().map(PathBuf::from), Some(candidate));
        }
        assert!(!u.dir().join("runner").join(format!("{id}-ops")).exists());
    }
}
