//! Shared read-only OS sandbox policy; Research keeps its tool/scope checks separately.
#[cfg(test)]
mod tests {
    use steve_confine::{SANDBOX_EXEC, available, readonly_profile as profile};

    #[test]
    fn profile_shape() {
        let p = profile(&["/s/run".into(), "/h/.claude".into()]).unwrap();
        assert!(p.contains("(deny file-write*)"));
        assert!(p.contains("(subpath \"/s/run\") (subpath \"/h/.claude\")"));
        assert!(profile(&["/a\"b".into()]).is_none());
        assert!(profile(&["rel".into()]).is_none());
    }

    /// Layer 3 on its own: a process that ignores every allowlist still cannot write outside the
    /// writable directories.
    #[cfg(target_os = "macos")]
    #[test]
    fn writes_outside_the_profile_fail() {
        if !available() {
            eprintln!("skipped: no seatbelt here (inside a sandbox already?)");
            return;
        }
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let proj = root.join("proj");
        let run = root.join("run");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(proj.join("keep.txt"), "orig").unwrap();
        let prof = profile(std::slice::from_ref(&run)).unwrap();
        let pf = run.join("p.sb");
        std::fs::write(&pf, prof).unwrap();
        let script = format!(
            "echo x > '{p}/new.txt'; echo y > '{p}/keep.txt'; rm -f '{p}/keep.txt'; echo ok > '{r}/fine.txt'",
            p = proj.display(),
            r = run.display()
        );
        let st = std::process::Command::new(SANDBOX_EXEC)
            .arg("-f")
            .arg(&pf)
            .args(["/bin/sh", "-c", &script])
            .status()
            .unwrap();
        assert!(st.success(), "last command (write in run dir) succeeds");
        assert!(!proj.join("new.txt").exists());
        assert_eq!(
            std::fs::read_to_string(proj.join("keep.txt")).unwrap(),
            "orig"
        );
        assert_eq!(
            std::fs::read_to_string(run.join("fine.txt")).unwrap(),
            "ok\n"
        );
    }
}
