//! Release scripts run in their own process group so they outlive the Hub they restart. In tests nothing
//! restarts: a script a test started must be gone when its kit drops, and a fake one left behind must stop by itself.

use crate::git::tests::{branch_with, g, repo};
use crate::patch;
use crate::release::test_spawned;
use crate::repo::{Release, WS};
use crate::testkit::Kit;
use std::time::{Duration, Instant};

fn group_alive(pgid: u32) -> bool {
    let g = i32::try_from(pgid).unwrap();
    // SAFETY: signal 0 only checks that the group exists.
    unsafe { libc::kill(-g, 0) == 0 }
}

fn until(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    let mut ready = false;
    steve_runtime::testkit::eventually_blocking(
        "release condition or its original deadline",
        || {
            ready = f();
            ready || Instant::now() >= end
        },
    );
    ready
}

#[test]
fn a_release_script_does_not_outlive_its_test_kit() {
    let k = Kit::new();
    let script = k.home().join("release.sh");
    // The child of the script holds the group open but exits after 30 s if cleanup fails.
    std::fs::write(
        &script,
        "/bin/sleep 30 & child=$!\n: > \"$STEVE_HOME/running\"\nwait \"$child\"\n",
    )
    .unwrap();
    let rel = Release {
        kind: "script".into(),
        cmd: script.to_string_lossy().into(),
    };
    let p = k
        .db()
        .project_create(
            WS,
            "R",
            "",
            &k.home().to_string_lossy(),
            "dir",
            false,
            Some("dev"),
            Some(&rel),
        )
        .unwrap()
        .id;
    let i = k.item("wait", "收尾", Some(&p));
    k.db().item_patch(&i, patch![branch: "steve/x"]).unwrap();
    let s = k.session(&i, "idle");
    let r = k.decision(&i, Some(&s), "approve", "release", &["yes", "no"]);
    steve_store::core_fixture::decision(
        k.db().store(),
        &r,
        steve_store::core_fixture::DecisionState::Subject("steve/x"),
    );
    k.core.resolve(&r, "yes", "", "me").unwrap();
    let groups = test_spawned::of(k.home());
    assert_eq!(groups.len(), 1, "one release script started");
    let g = groups[0];
    assert!(
        until(5, || k.home().join("running").is_file()),
        "the release script did not start its child"
    );
    assert!(
        group_alive(g),
        "the script keeps running while the kit lives"
    );

    drop(k);
    assert!(
        until(5, || !group_alive(g)),
        "no process of the release script survives the kit"
    );
}

#[test]
fn a_fake_release_script_stops_when_its_home_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let script = dir.path().join("release.sh");
    std::fs::write(
        &script,
        format!(
            ": > \"$STEVE_HOME/waiting\" || exit 2\n{}",
            super::fake_release_script("exit 0\n")
        ),
    )
    .unwrap();
    let mut cmd = steve_runtime::testkit::process::Command::new("/bin/zsh");
    cmd.arg(&script).env("STEVE_HOME", &home);
    let mut child = cmd.spawn().unwrap();
    assert!(
        until(3, || home.join("waiting").is_file()),
        "the fake release script did not reach its wait loop"
    );
    assert!(child.try_wait().unwrap().is_none(), "still waiting for go");
    std::fs::remove_dir_all(&home).unwrap();
    let mut status = child.try_wait().unwrap();
    if !until(3, || {
        if status.is_none() {
            status = child.try_wait().unwrap();
        }
        status.is_some()
    }) {
        panic!("the script did not exit once its home was deleted");
    }
    assert_eq!(status.unwrap().code(), Some(1));
}

/// The release script is a Hub-host operation (D12): a project on a remote Node is refused with a fail card,
/// and neither its release nor a rollback starts a process, even when a script exists at its path here. Its
/// paths are the other device's: a checkout at the same path on the Hub, behind main, is not rebased.
#[test]
fn a_project_on_a_remote_device_is_refused_and_starts_no_script() {
    let k = Kit::new();
    let r = repo(k.home());
    let script = k.home().join("release.sh");
    std::fs::write(&script, super::fake_release_script("exit 0\n")).unwrap();
    let rel = Release {
        kind: "script".into(),
        cmd: script.to_string_lossy().into(),
    };
    let p = k
        .db()
        .project_create(
            WS,
            "R",
            "",
            &r.to_string_lossy(),
            "git",
            true,
            None,
            Some(&rel),
        )
        .unwrap()
        .id;
    steve_store::core_fixture::project_device(k.db().store(), &p, "mac-2");
    let i = k.item("wait", "收尾", Some(&p));
    let (name, wt) = branch_with(&r, &k.core.config().worktrees, &i, "b.txt", "b\n");
    std::fs::write(r.join("c.txt"), "c\n").unwrap();
    g(&r, &["add", "."]);
    g(&r, &["commit", "-qm", "main c"]);
    let head = g(&wt, &["rev-parse", "HEAD"]);
    k.db()
        .item_patch(
            &i,
            patch![cwd: wt.to_string_lossy().to_string(), branch: name.as_str()],
        )
        .unwrap();
    let s = k.session(&i, "idle");
    let r = k.decision(&i, Some(&s), "approve", "release", &["yes", "no"]);
    steve_store::core_fixture::decision(
        k.db().store(),
        &r,
        steve_store::core_fixture::DecisionState::Subject(&name),
    );
    k.core.resolve(&r, "yes", "", "me").unwrap();

    assert_eq!(
        g(&wt, &["rev-parse", "HEAD"]),
        head,
        "the checkout here is not rebased"
    );
    assert!(
        !k.timeline_texts(&i).iter().any(|t| t.contains("rebase")),
        "{:?}",
        k.timeline_texts(&i)
    );

    assert!(test_spawned::of(k.home()).is_empty(), "no script started");
    assert!(!k.core.release_busy());
    let res = k.db().decision(&r).unwrap().unwrap().result;
    assert_eq!(res["outcome"], "not_released");
    assert_eq!(
        res["reason"],
        "项目在设备 mac-2 上，发布脚本只能在 Hub 所在的设备上运行"
    );
    let status = k.item_label(&i);
    assert_eq!(status, "wait");
    assert_eq!(k.open_kinds(&i), vec!["fail"]);
    assert_eq!(
        k.open_fails(&i),
        vec!["这个项目在设备 mac-2 上；发布脚本只能在 Hub 所在的设备上运行，无法发布"]
    );

    let e = k.core.tx().start_rollback(&i, "previous", "").unwrap_err();
    assert!(e.to_string().contains("mac-2"), "{e}");
    assert!(test_spawned::of(k.home()).is_empty(), "no rollback started");
    assert!(!k.core.release_busy());
}
