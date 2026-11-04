//! Release scripts run in their own process group so they outlive the Hub they restart. In tests nothing
//! restarts: a script a test started must be gone when its kit drops, and a fake one left behind must stop by itself.

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

fn until(secs: u64, f: impl Fn() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    f()
}

#[test]
fn a_release_script_does_not_outlive_its_test_kit() {
    let k = Kit::new();
    let script = k.home().join("release.sh");
    /* ignores its directory going away (only the kit can stop it in time; it gives up after ~30 s so a failing
     * run leaves nothing for long), with a child of its own in the group */
    std::fs::write(
        &script,
        "n=0\nwhile [ $n -lt 600 ]; do n=$((n+1)); sleep 0.05 & wait; done\n",
    )
    .unwrap();
    let rel = Release {
        kind: "script".into(),
        cmd: script.to_string_lossy().into(),
        done_when: None,
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
    k.db()
        .exec(
            "UPDATE decisions SET subject_id = 'steve/x' WHERE id = ?",
            &[&r],
        )
        .unwrap();
    k.core.resolve(&r, "yes", "", "me").unwrap();
    assert_eq!(k.status(&i).2, "正在发布");
    let groups = test_spawned::of(k.home());
    assert_eq!(groups.len(), 1, "one release script started");
    let g = groups[0];
    std::thread::sleep(Duration::from_millis(300));
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
    std::fs::write(&script, super::fake_release_script("exit 0\n")).unwrap();
    let mut cmd = std::process::Command::new("/bin/zsh");
    cmd.arg(&script).env("STEVE_HOME", &home);
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd.spawn().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(child.try_wait().unwrap().is_none(), "still waiting for go");
    std::fs::remove_dir_all(&home).unwrap();
    let end = Instant::now() + Duration::from_secs(3);
    let mut status = child.try_wait().unwrap();
    while status.is_none() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
        status = child.try_wait().unwrap();
    }
    if status.is_none() {
        let _ = child.kill();
        panic!("the script did not exit once its home was deleted");
    }
    assert_eq!(status.unwrap().code(), Some(1));
}
