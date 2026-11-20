use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use steve_node_cli::config::{Home, Pairing};
use steve_node_wire::{Identity, identity};
use steve_runtime::testkit::process::{AsyncCommand, Command};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn command(home: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_steve-node"));
    c.env("STEVE_HOME", home)
        .env("HOME", home)
        .env("HOSTNAME", "Fixture host");
    c
}

#[test]
fn cli_status_forget_and_usage_keep_the_existing_outputs_and_exit_codes() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home(dir.path().join("node-home"));
    std::fs::create_dir_all(home.identity().parent().unwrap()).unwrap();
    let key = Identity::generate().unwrap();
    key.save(&home.identity()).unwrap();
    let fp = identity::grouped(&key.fingerprint());
    let p = Pairing {
        hub_url: "https://hub.example".into(),
        node_id: "node-fixture".into(),
        hub_id: "hub-fixture".into(),
        name: "Display name".into(),
        pin: None,
        hub_key: None,
        loopback: false,
    };
    home.save_pairing(&p).unwrap();
    std::fs::write(home.0.join("keep"), "unrelated").unwrap();
    let out = command(&home.0).arg("status").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!(
            "node node-fixture (Display name) paired with hub-fixture at https://hub.example\nfingerprint {fp}\n"
        )
    );
    std::fs::remove_file(home.pairing_file()).unwrap();
    let out = command(&home.0).arg("status").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!(
            "not paired yet ({} is missing)\nfingerprint {fp}\n",
            home.pairing_file().display()
        )
    );
    home.save_pairing(&p).unwrap();
    let out = command(&home.0).arg("forget").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!(
            "removed {}\nremoved {}\n",
            home.pairing_file().display(),
            home.identity().display()
        )
    );
    assert!(home.0.join("keep").is_file());
    let out = command(&home.0).arg("forget").output().unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    for args in [
        vec![],
        vec!["unknown"],
        vec!["pair"],
        vec!["upgrade-switch"],
        vec!["remove-finish"],
        vec!["pair", "http://127.0.0.1:1", "--unknown"],
    ] {
        let out = command(&home.0).args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
        assert!(out.stdout.is_empty());
        assert!(
            String::from_utf8(out.stderr)
                .unwrap()
                .starts_with("usage: steve-node pair <hub-url>"),
            "{args:?}"
        );
    }
    // The missing pairing is reported before a Node can start or rotate.
    for arg in ["run", "rotate"] {
        let out = command(&home.0).arg(arg).output().unwrap();
        assert!(!out.status.success());
        assert!(
            String::from_utf8(out.stderr)
                .unwrap()
                .contains("not paired yet")
        );
    }
}

#[test]
fn forget_stops_on_its_first_io_failure_without_deleting_the_identity() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home(dir.path().into());
    std::fs::create_dir_all(home.pairing_file()).unwrap();
    std::fs::write(home.identity(), b"retained").unwrap();
    let out = command(&home.0).arg("forget").output().unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert_eq!(std::fs::read(home.identity()).unwrap(), b"retained");
}

async fn request(socket: &mut tokio::net::TcpStream) -> Value {
    let mut header = vec![];
    while !header.ends_with(b"\r\n\r\n") {
        assert!(header.len() < 16_384);
        header.push(socket.read_u8().await.unwrap());
    }
    let header = String::from_utf8(header).unwrap();
    assert!(header.starts_with("POST /node/pair HTTP/1.1"));
    let length = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    let mut body = vec![0; length];
    socket.read_exact(&mut body).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn pair_parsing_code_sources_and_fingerprint_use_the_real_cli_and_http_request() {
    for (env_code, stdin, expected_code, args, expected_name) in [
        (
            Some(" fixture-code "),
            "ignored",
            "fixture-code",
            vec!["--name", "Chosen name", "--name"],
            "Chosen name",
        ),
        (Some(" "), "ignored", "", vec!["--pin"], "Fixture host"),
        (None, " stdin-code \n", "stdin-code", vec![], "Fixture host"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let home = Home(dir.path().join("node-home"));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert_ne!(port, 8440);
        let server = tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let body = request(&mut tcp).await;
            let reply =
                json!({"node_id":"paired-node","hub_id":"paired-hub","hub_key":"a".repeat(64)})
                    .to_string();
            tcp.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",reply.len()).as_bytes()).await.unwrap();
            body
        });
        let mut c = AsyncCommand::new(env!("CARGO_BIN_EXE_steve-node"));
        c.arg("pair")
            .arg(format!("http://127.0.0.1:{port}/"))
            .args(args)
            .env("STEVE_HOME", &home.0)
            .env("HOME", dir.path())
            .env("HOSTNAME", "Fixture host")
            .env("STEVE_NODE_LOOPBACK_PLAINTEXT", "1")
            .env_remove("STEVE_PAIR_CODE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(code) = env_code {
            c.env("STEVE_PAIR_CODE", code);
        }
        let mut child = c.spawn().unwrap();
        let mut input = child.stdin.take().unwrap();
        input.write_all(stdin.as_bytes()).await.unwrap();
        drop(input);
        let out = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let request = server.await.unwrap();
        assert_eq!(request["code"], expected_code);
        assert_eq!(request["name"], expected_name);
        let identity = Identity::load(&home.identity()).unwrap();
        assert_eq!(request["pubkey"], hex::encode(identity.public()));
        let sig: [u8; 64] = hex::decode(request["sig"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        assert!(steve_node_wire::identity::verify(
            &identity.public(),
            &steve_node_wire::identity::pair_message(expected_code, &identity.public()),
            &sig
        ));
        let saved = home.pairing().unwrap();
        assert_eq!(saved.hub_url, format!("http://127.0.0.1:{port}"));
        assert_eq!(saved.name, expected_name);
        assert_eq!(saved.pin, None);
        assert_eq!(saved.hub_key.as_deref(), Some("a".repeat(64).as_str()));
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            format!(
                "paired as paired-node with hub paired-hub\nfingerprint {}\nconfirm this fingerprint on the hub (Devices, Add device: paste it or type its eight groups), then run: steve-node run\n",
                identity::grouped(&identity.fingerprint())
            )
        );
    }
}
