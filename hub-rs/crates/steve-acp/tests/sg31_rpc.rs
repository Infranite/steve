//! Public RPC process lifecycle, admission and backpressure, using a finite isolated peer.
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use steve_acp::rpc::{Backpressure, Conn, Incoming, Spawn};
fn python() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|p| p.join("python3"))
        .find(|p| p.is_file())
        .unwrap()
}
fn spawn_spec(dir: &Path, case: &str, prefix: Option<PathBuf>) -> Spawn {
    let path = dir.join("peer.py");
    let script=r#"import json,sys
case=CASE
def send(v): print(json.dumps({'jsonrpc':'2.0',**v}),flush=True)
for line in sys.stdin:
 m=json.loads(line);method=m['method']
 if method=='exit': sys.exit(7)
 if method=='fail': send({'id':m['id'],'error':{'code':-32012,'message':'scripted failure','data':{'retry':False}}});continue
 if method=='pressure':
  send({'method':'notice','params':{'n':1}});send({'method':'notice','params':{'n':2}})
  send({'id':m['id'],'result':{'ok':True}})
  if case=='exit-blocked': sys.exit(7)
  continue
 if method=='large': send({'id':m['id'],'result':'x'*800});continue
 print('not-json',flush=True);print('',flush=True)
 send({'method':'notice','params':{'n':3}})
 send({'id':m['id'],'result':m['params']})
"#.replace("CASE",&serde_json::to_string(case).unwrap());
    std::fs::write(&path, format!("#!{}\n{script}", python().display())).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    Spawn {
        program: path,
        args: vec![],
        cwd: dir.into(),
        env: vec![],
        log_tag: "sg31-rpc".into(),
        clean_env: false,
        prefix,
    }
}
struct Reap(Arc<Conn>);
impl Drop for Reap {
    fn drop(&mut self) {
        self.0.kill();
    }
}
#[tokio::test]
async fn sg31_rpc_dispatches_errors_notifications_and_reports_the_real_exit_without_parse_leaks() {
    let d = tempfile::tempdir().unwrap();
    let notices = Arc::new(Mutex::new(Vec::new()));
    let ns = notices.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let c = Conn::spawn_bounded_observed(
        spawn_spec(d.path(), "normal", None),
        4096,
        |_| true,
        move |_, inc| {
            if let Incoming::Notification { method, params } = inc {
                ns.lock().unwrap().push((method, params));
            }
            None
        },
        move |r| {
            let _ = tx.send(r);
        },
    )
    .unwrap();
    let _reap = Reap(c.clone());
    let v = c
        .request(
            "echo",
            json!({"array":[1,"two"],"unicode":"字"}),
            Some(Duration::from_secs(3)),
        )
        .await
        .unwrap();
    assert_eq!(v, json!({"array":[1,"two"],"unicode":"字"}));
    assert_eq!(
        *notices.lock().unwrap(),
        vec![("notice".into(), json!({"n":3}))]
    );
    let e = c.request("fail", json!({}), None).await.unwrap_err();
    assert_eq!(e.message, "scripted failure");
    assert_eq!(
        e.data.unwrap(),
        json!({"code":-32012,"message":"scripted failure","data":{"retry":false}})
    );
    let e = c.request("exit", Value::Null, None).await.unwrap_err();
    assert_eq!(e.message, "adapter exited (7)");
    let r = tokio::time::timeout(Duration::from_secs(3), rx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.exit_code, Some(7));
    assert_eq!(r.signal, None);
    assert!(!r.wait_error && !r.reader_failed && !r.drain_timed_out);
    assert_eq!(r.parse_errors, 1);
    c.wait_exit().await;
}
#[tokio::test]
async fn sg31_rpc_frame_ceiling_and_admission_refusal_fail_pending_before_parsing() {
    for hard_limit in [false, true] {
        let d = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let ss = seen.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let c = Conn::spawn_bounded_observed(
            spawn_spec(d.path(), "normal", None),
            if hard_limit { 128 } else { 4096 },
            move |n| {
                ss.lock().unwrap().push(n);
                hard_limit
            },
            |_, _| None,
            move |r| {
                let _ = tx.send(r);
            },
        )
        .unwrap();
        let _reap = Reap(c.clone());
        let e = c
            .request("large", Value::Null, Some(Duration::from_secs(3)))
            .await
            .unwrap_err();
        assert_eq!(e.message, "adapter exited: frame limit");
        assert!(c.has_exited());
        let sizes = seen.lock().unwrap().clone();
        assert_eq!(sizes.len(), 1);
        if hard_limit {
            assert_eq!(sizes[0], 129);
        } else {
            assert!(sizes[0] > 128 && sizes[0] < 4096);
        }
        c.kill();
        let r = tokio::time::timeout(Duration::from_secs(3), rx)
            .await
            .unwrap()
            .unwrap();
        assert!(r.reader_failed);
        assert_eq!(r.parse_errors, 0);
        c.wait_exit().await;
    }
}
struct Dropped(Arc<AtomicBool>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[tokio::test]
async fn sg31_rpc_backpressure_is_ordered_and_exit_cancels_and_joins_the_waiter_without_releasing_the_lease_early()
 {
    for exit_blocked in [false, true] {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let (ss, ee, rr, dd) = (
            seen.clone(),
            entered.clone(),
            release.clone(),
            dropped.clone(),
        );
        let (tx, rx) = tokio::sync::oneshot::channel();
        let c = Conn::spawn_bounded_observed(
            spawn_spec(
                d.path(),
                if exit_blocked {
                    "exit-blocked"
                } else {
                    "normal"
                },
                Some(home.clone()),
            ),
            4096,
            |_| true,
            move |_, inc| -> Option<Backpressure> {
                if let Incoming::Notification { params, .. } = inc {
                    let n = params["n"].as_u64().unwrap();
                    ss.lock().unwrap().push(n);
                    if n == 1 {
                        let (e, r, d) = (ee.clone(), rr.clone(), dd.clone());
                        return Some(Box::pin(async move {
                            let _guard = Dropped(d);
                            e.notify_one();
                            r.notified().await;
                        }));
                    }
                }
                None
            },
            move |r| {
                let _ = tx.send(r);
            },
        )
        .unwrap();
        let reap = Reap(c.clone());
        assert!(steve_acp::managed::prefix_lock(&home, true).is_err());
        let cc = c.clone();
        let request = tokio::spawn(async move { cc.request("pressure", Value::Null, None).await });
        tokio::time::timeout(Duration::from_secs(3), entered.notified())
            .await
            .unwrap();
        assert_eq!(*seen.lock().unwrap(), [1]);
        assert!(!request.is_finished());
        if !exit_blocked {
            release.notify_one();
            assert_eq!(request.await.unwrap().unwrap(), json!({"ok":true}));
            assert_eq!(*seen.lock().unwrap(), [1, 2]);
            c.kill();
        } else {
            assert_eq!(
                request.await.unwrap().unwrap_err().message,
                "adapter exited (7)"
            );
        }
        let r = tokio::time::timeout(Duration::from_secs(3), rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.drain_timed_out, exit_blocked);
        assert!(!r.reader_failed);
        assert_eq!(r.parse_errors, 0);
        if exit_blocked {
            assert_eq!(r.exit_code, Some(7));
        }
        assert!(dropped.load(Ordering::SeqCst));
        c.wait_exit().await;
        assert!(
            steve_acp::managed::prefix_lock(&home, true).is_err(),
            "retained Conn still owns the prefix"
        );
        drop(reap);
        drop(c);
        assert!(steve_acp::managed::prefix_lock(&home, true).is_ok());
    }
}
#[tokio::test]
async fn sg31_rpc_exclusive_prefix_prevents_spawn_and_an_invalid_program_keeps_spawn_error_semantics()
 {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let lease = steve_acp::managed::prefix_lock(&home, true).unwrap();
    let exited = Arc::new(AtomicBool::new(false));
    let ee = exited.clone();
    let e = Conn::spawn_bounded_observed(
        spawn_spec(d.path(), "normal", Some(home.clone())),
        4096,
        |_| true,
        |_, _| None,
        move |_| {
            ee.store(true, Ordering::SeqCst);
        },
    )
    .err()
    .unwrap();
    assert!(e.message.contains("prefix is in use"));
    assert!(!exited.load(Ordering::SeqCst));
    drop(lease);
    let mut s = spawn_spec(d.path(), "normal", Some(home.clone()));
    s.program = d.path().join("missing-program");
    let e = Conn::spawn_bounded_observed(s, 4096, |_| true, |_, _| None, |_| {})
        .err()
        .unwrap();
    assert!(e.message.starts_with("spawn "));
    assert!(e.data.is_none());
    assert!(
        steve_acp::managed::prefix_lock(&home, true).is_ok(),
        "failed spawn releases the lease"
    );
}
