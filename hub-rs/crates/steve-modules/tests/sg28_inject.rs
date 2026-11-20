//! SG28: zero-delay command fixtures for injection, failure cleanup and capped collection.
use std::time::{Duration, Instant};
use steve_modules::vault::inject::{self, How, Injection, OUTPUT_CAP, RunSpec};
use steve_modules::vault::secret::SecretBytes;
fn spec(cwd: &std::path::Path, command: &str, injections: Vec<Injection>) -> RunSpec {
    RunSpec {
        use_id: steve_store::uid("su"),
        cwd: cwd.into(),
        command: command.into(),
        injections,
        timeout: Duration::from_secs(3),
    }
}
fn inj(var: &str, label: &str, how: How) -> Injection {
    Injection {
        var: var.into(),
        label: label.into(),
        how,
    }
}
fn values(xs: &[(&str, &[u8])]) -> Vec<(String, SecretBytes)> {
    xs.iter()
        .map(|(k, v)| (k.to_string(), SecretBytes::new(v.to_vec())))
        .collect()
}
#[tokio::test]
async fn sg28_env_file_stdin_and_last_stdin_value() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let s = spec(
        cwd.path(),
        "printf 'env=%s\\n' \"$E\"; printf 'file='; cat \"$F\"; printf '\\nstdin='; cat; printf 'warning' >&2",
        vec![
            inj("E", "env", How::Env),
            inj("F", "file", How::File),
            inj("", "first", How::Stdin),
            inj("", "last", How::Stdin),
        ],
    );
    let v = values(&[
        ("env", b"value"),
        ("file", b"file-data"),
        ("first", b"unused"),
        ("last", b"last-data"),
    ]);
    let o = inject::run(home.path(), &s, &v).await;
    assert_eq!(o.exit_code, Some(0), "{o:?}");
    assert!(!o.timed_out);
    assert_eq!(o.error, None);
    assert_eq!(o.stdout, "env=value\nfile=file-data\nstdin=last-data");
    assert_eq!(o.stderr, "warning");
    assert_eq!(
        o.combined(),
        "env=value\nfile=file-data\nstdin=last-data\n[stderr]\nwarning"
    );
    assert!(!inject::secrets_root(home.path()).join(&s.use_id).exists());
}
#[tokio::test]
async fn sg28_missing_values_and_duplicate_file_fail_before_spawn_and_cleanup() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    for how in [How::Env, How::File, How::Stdin] {
        let s = spec(cwd.path(), "touch started", vec![inj("F", "missing", how)]);
        let o = inject::run(home.path(), &s, &[]).await;
        assert_eq!(o.error.as_deref(), Some("field missing has no value"));
        assert_eq!(o.exit_code, None);
        assert!(!o.timed_out);
        assert!(!cwd.path().join("started").exists());
        assert!(!inject::secrets_root(home.path()).join(&s.use_id).exists());
    }
    let s = spec(
        cwd.path(),
        "touch started",
        vec![inj("F", "v", How::File), inj("F", "v", How::File)],
    );
    let o = inject::run(home.path(), &s, &values(&[("v", b"value")])).await;
    assert!(
        o.error
            .unwrap()
            .starts_with("could not write the temp file:")
    );
    assert!(!cwd.path().join("started").exists());
    assert!(!inject::secrets_root(home.path()).join(&s.use_id).exists());
}
#[tokio::test]
async fn sg28_temp_creation_and_process_start_failures_have_empty_streams() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mut s = spec(cwd.path(), "printf never", vec![inj("F", "v", How::File)]);
    s.use_id = "../invalid".into();
    let o = inject::run(home.path(), &s, &values(&[("v", b"value")])).await;
    assert_eq!(
        o.error.as_deref(),
        Some("could not create the temp directory: bad use id")
    );
    assert_eq!(o.stdout, "");
    assert_eq!(o.stderr, "");
    let s = spec(
        &cwd.path().join("missing"),
        "printf never",
        vec![inj("F", "v", How::File)],
    );
    let o = inject::run(home.path(), &s, &values(&[("v", b"value")])).await;
    assert!(o.error.unwrap().starts_with("could not start the command:"));
    assert_eq!(o.exit_code, None);
    assert!(!o.timed_out);
    assert!(o.stdout.is_empty() && o.stderr.is_empty());
    assert!(!inject::secrets_root(home.path()).join(&s.use_id).exists());
}
#[tokio::test]
async fn sg28_exit_and_signal_table() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    for (cmd, code) in [
        ("exit 0", Some(0)),
        ("exit 7", Some(7)),
        ("kill -TERM $$", None),
    ] {
        let o = inject::run(home.path(), &spec(cwd.path(), cmd, vec![]), &[]).await;
        assert_eq!(o.exit_code, code);
        assert_eq!(o.ok(), code == Some(0));
        assert!(!o.timed_out);
        assert_eq!(o.error, None);
        assert_eq!(o.code(), code.unwrap_or(-1));
    }
}
#[tokio::test]
async fn sg28_utf8_cap_is_independent_for_both_streams() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let payload = "界".repeat(OUTPUT_CAP / 3 + 5);
    let s = spec(
        cwd.path(),
        "printf '%s' \"$P\"; printf '%s' \"$P\" >&2",
        vec![inj("P", "payload", How::Env)],
    );
    let o = inject::run(home.path(), &s, &values(&[("payload", payload.as_bytes())])).await;
    assert_eq!(o.exit_code, Some(0), "{o:?}");
    let expected = "界".repeat(OUTPUT_CAP / 3);
    assert_eq!(o.stdout, expected);
    assert_eq!(o.stderr, expected);
}
#[tokio::test]
async fn sg28_timeout_and_straggler_cleanup_do_not_wait_for_pipe_eof() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mut s = spec(
        cwd.path(),
        "while :; do :; done",
        vec![inj("F", "v", How::File)],
    );
    s.timeout = Duration::from_millis(40);
    let before = Instant::now();
    let o = inject::run(home.path(), &s, &values(&[("v", b"value")])).await;
    assert!(o.timed_out);
    assert_eq!(o.exit_code, None);
    assert_eq!(
        o.error.as_deref(),
        Some("timed out after 0 s; the command was stopped")
    );
    assert!(before.elapsed() < Duration::from_secs(3));
    assert!(!inject::secrets_root(home.path()).join(&s.use_id).exists());
    let s = spec(
        cwd.path(),
        "printf ready; (while :; do :; done) & exit 0",
        vec![],
    );
    let before = Instant::now();
    let o = inject::run(home.path(), &s, &[]).await;
    assert_eq!(o.exit_code, Some(0));
    assert_eq!(o.stdout, "ready");
    assert!(before.elapsed() < Duration::from_secs(2));
}
