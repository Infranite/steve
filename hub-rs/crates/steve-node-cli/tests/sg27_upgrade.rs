use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use steve_node_cli::upgrade::{self, ServiceOps};

fn path(home: &Path) -> PathBuf {
    home.join(upgrade::DIR).join("sg27-switch.json")
}
fn read(home: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path(home)).unwrap()).unwrap()
}
fn save(home: &Path, r: &Value) {
    std::fs::create_dir_all(home.join(upgrade::DIR)).unwrap();
    std::fs::write(path(home), serde_json::to_vec(r).unwrap()).unwrap();
}
fn record(home: &Path) -> Value {
    json!({
        "state":{"operation":"sg27-switch","phase":"switching","version":"1.2.3","previous":"1.0.0","error":null,"updated":1},
        "request":{"operation":"sg27-switch","build":{"version":"1.2.3","commit":"a".repeat(40),"target":env!("STEVE_BUILD_TARGET")},
            "artifact":"sg27-archive","sha256":"0".repeat(64),"size":1,"health_secs":1},
        "release":home.join("releases/new"),"previous_release":home.join("releases/old")
    })
}
struct Service {
    home: PathBuf,
    current: PathBuf,
    calls: Vec<String>,
    stops: VecDeque<bool>,
    updates: VecDeque<bool>,
    starts: VecDeque<bool>,
    link: bool,
}
impl Service {
    fn new(home: &Path) -> Self {
        Self {
            home: home.into(),
            current: home.join("releases/old"),
            calls: vec![],
            stops: VecDeque::new(),
            updates: VecDeque::new(),
            starts: VecDeque::new(),
            link: true,
        }
    }
}
impl ServiceOps for Service {
    fn stop(&mut self) -> anyhow::Result<()> {
        self.calls.push("stop".into());
        anyhow::ensure!(!self.stops.pop_front().unwrap_or(false), "stop refused");
        Ok(())
    }
    fn update(&mut self, release: &Path) -> anyhow::Result<()> {
        self.calls.push(format!(
            "update {}",
            release.file_name().unwrap().to_str().unwrap()
        ));
        anyhow::ensure!(!self.updates.pop_front().unwrap_or(false), "update refused");
        self.current = release.into();
        Ok(())
    }
    fn start(&mut self) -> anyhow::Result<()> {
        self.calls.push("start".into());
        anyhow::ensure!(!self.starts.pop_front().unwrap_or(false), "start refused");
        if self.link && self.current.ends_with("new") {
            let mut r = read(&self.home);
            r["state"]["phase"] = json!("linked");
            save(&self.home, &r);
        }
        Ok(())
    }
}

#[test]
fn switch_success_reloads_the_linked_record_and_cannot_be_repeated() {
    let home = tempfile::tempdir().unwrap();
    let mut r = record(home.path());
    r["state"]["previous"] = json!("retained by reload");
    save(home.path(), &r);
    let mut svc = Service::new(home.path());
    let state = upgrade::run_switch(home.path(), "sg27-switch", &mut svc, Duration::ZERO).unwrap();
    assert_eq!(svc.calls, ["stop", "update new", "start"]);
    assert_eq!(state.phase, "succeeded");
    assert_eq!(state.previous.as_deref(), Some("retained by reload"));
    assert!(state.error.is_none());
    assert_eq!(read(home.path())["state"]["phase"], "succeeded");
    let calls = svc.calls.clone();
    assert!(
        upgrade::run_switch(home.path(), "sg27-switch", &mut svc, Duration::ZERO)
            .unwrap_err()
            .to_string()
            .contains("not switching")
    );
    assert_eq!(svc.calls, calls);
}

#[test]
fn switch_failures_keep_forward_and_rollback_short_circuit_order_and_diagnostics() {
    for (case, calls, phase, message) in [
        (
            "stop",
            vec!["stop", "start"],
            "failed",
            "stopping the node: stop refused",
        ),
        (
            "update",
            vec!["stop", "update new", "start"],
            "failed",
            "switching the release: update refused",
        ),
        (
            "start",
            vec!["stop", "update new", "start", "stop", "update old", "start"],
            "rolled_back",
            "the new release did not start: start refused",
        ),
        (
            "rollback-stop",
            vec!["stop", "update new", "start", "stop"],
            "failed",
            "the new release did not start: start refused; going back failed too: stop refused",
        ),
        (
            "rollback-update",
            vec!["stop", "update new", "start", "stop", "update old"],
            "failed",
            "the new release did not start: start refused; going back failed too: update refused",
        ),
        (
            "rollback-start",
            vec!["stop", "update new", "start", "stop", "update old", "start"],
            "failed",
            "the new release did not start: start refused; going back failed too: start refused",
        ),
    ] {
        let home = tempfile::tempdir().unwrap();
        save(home.path(), &record(home.path()));
        let mut svc = Service::new(home.path());
        match case {
            "stop" => svc.stops = vec![true].into(),
            "update" => svc.updates = vec![true].into(),
            "start" => svc.starts = vec![true, false].into(),
            "rollback-stop" => {
                svc.starts = vec![true].into();
                svc.stops = vec![false, true].into();
            }
            "rollback-update" => {
                svc.starts = vec![true].into();
                svc.updates = vec![false, true].into();
            }
            "rollback-start" => svc.starts = vec![true, true].into(),
            _ => unreachable!(),
        }
        let state =
            upgrade::run_switch(home.path(), "sg27-switch", &mut svc, Duration::ZERO).unwrap();
        assert_eq!(svc.calls, calls, "{case}");
        assert_eq!(state.phase, phase, "{case}");
        assert_eq!(state.error.as_deref(), Some(message), "{case}");
        assert_eq!(read(home.path())["state"]["phase"], phase);
    }
}

#[test]
fn switch_requires_its_phase_and_both_release_paths_before_calling_service() {
    for (field, value, message) in [
        ("phase", json!("staged"), "not switching"),
        ("release", Value::Null, "nothing staged"),
        ("previous_release", Value::Null, "no release to go back to"),
    ] {
        let home = tempfile::tempdir().unwrap();
        let mut r = record(home.path());
        if field == "phase" {
            r["state"][field] = value;
        } else {
            r[field] = value;
        }
        save(home.path(), &r);
        let mut svc = Service::new(home.path());
        let error =
            upgrade::run_switch(home.path(), "sg27-switch", &mut svc, Duration::ZERO).unwrap_err();
        assert!(error.to_string().contains(message), "{error:#}");
        assert!(svc.calls.is_empty());
        assert_eq!(read(home.path()), r);
    }
}
