use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use steve_runtime::testkit::eventually_blocking;

struct Control(BufReader<UnixStream>);

impl Control {
    fn accept(listener: &UnixListener) -> Self {
        let mut stream = None;
        eventually_blocking(
            "The gated fixture connects to its test-owned socket.",
            || match listener.accept() {
                Ok((s, _)) => {
                    stream = Some(s);
                    true
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => false,
                Err(e) => panic!("accept fixture: {e}"),
            },
        );
        let stream = stream.unwrap();
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        Self(BufReader::new(stream))
    }

    fn read(&mut self) -> String {
        let mut line = String::new();
        assert!(
            self.0.read_line(&mut line).expect("gate reply") > 0,
            "fixture disconnected"
        );
        line.trim().to_string()
    }

    fn send(&mut self, command: &str) {
        writeln!(self.0.get_mut(), "{command}").expect("gate command");
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        let _ = self.0.get_mut().shutdown(Shutdown::Both);
    }
}

fn fake(h: &Home, role: &str, gate: &std::path::Path, log: &std::path::Path) -> Child {
    Command::new("node")
        .args([
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/late-spawn.mjs"),
            role,
            gate.to_str().unwrap(),
        ])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").expect("test PATH"))
        .env("HOME", h.dir.path())
        .env("FAKE_ACP_LOG", log)
        .current_dir(h.dir.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap()
}

#[test]
fn late_old_adapter_does_not_count_as_new_hub_spawn() {
    let h = Home::new();
    let gate = h.dir.path().join("gate.sock");
    let listener = UnixListener::bind(&gate).unwrap();
    listener.set_nonblocking(true).unwrap();
    let old_log = h.adapter_log();
    let child = fake(&h, "parent", &gate, &old_log);
    let mut old = Hub(Some(child), old_log);
    let mut parent = Control::accept(&listener);
    assert!(parent.read().starts_with("parent "));
    let mut adapter = None;
    // Make the real discovery-to-signal window deterministic, rather than hoping load hits it.
    Hub::kill_child_after_scan(old.0.take().unwrap(), |children| {
        parent.send("spawn");
        let mut late = Control::accept(&listener);
        let ready = late.read();
        let pid: i32 = ready.strip_prefix("adapter ").unwrap().parse().unwrap();
        assert!(
            !children.contains(&pid),
            "the adapter starts after enumeration"
        );
        adapter = Some(late);
    });
    assert!(
        old.0.is_none(),
        "the old Hub was reaped before taking the snapshot"
    );
    let spawns = h.spawns();
    let hub = spawn(&h, false, &[]);
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(serving(&h));
    let mut adapter = adapter.unwrap();
    adapter.send("log");
    assert_eq!(
        adapter.read(),
        "logged",
        "the old fake appends after the snapshot"
    );
    drop(adapter);
    assert_eq!(
        h.spawns(),
        spawns + 1,
        "the old record is still accounted for"
    );
    assert_eq!(old.spawns(), 1, "the late spawn belongs to the reaped Hub");
    assert_eq!(hub.spawns(), 0, "no adapter started on the Hub for it");

    // Positive control: an actual spawn in the new log must still fail the zero-spawn contract.
    let mut current = fake(&h, "adapter", &gate, &hub.1);
    let mut control = Control::accept(&listener);
    assert!(control.read().starts_with("adapter "));
    assert_eq!(hub.spawns(), 0, "this fake is also held before logging");
    control.send("log");
    assert_eq!(control.read(), "logged");
    drop(control);
    eventually_blocking(
        "The controlled current adapter exits when its gate closes.",
        || current.try_wait().unwrap().is_some(),
    );
    assert!(current.wait().unwrap().success());
    assert_eq!(hub.spawns(), 1, "a new-instance spawn cannot be hidden");
    assert_eq!(
        old.spawns(),
        1,
        "new logging does not change the old instance"
    );
}
