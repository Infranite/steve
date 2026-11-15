use super::*;
use std::io::{Cursor, Read, Write};

pub(super) fn fixture() -> (tempfile::TempDir, Service, steve_store::Session) {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path().join("test.sqlite3"), steve_store::NoPublisher).unwrap();
    store.ensure_workspace().unwrap();
    store.transaction(|t| {
        t.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES('ST-1','default',1,'delivery','run','work',1,1)",[])?;
        Ok::<_,StoreError>(())
    }).unwrap();
    let cwd = home.path().join("work");
    fs::create_dir(&cwd).unwrap();
    let session = store
        .sessions()
        .create(&steve_store::NewSession {
            workspace_id: "default".into(),
            item_id: Some("ST-1".into()),
            cwd: cwd.to_string_lossy().into_owned(),
            harness: "test".into(),
            title: "delivery".into(),
            token: "bot-token".into(),
            ..Default::default()
        })
        .unwrap();
    let svc = Service::new(
        store,
        home.path().to_path_buf(),
        "https://steve.example".into(),
    );
    (home, svc, session)
}

pub(super) fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        writer
            .start_file(
                *name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated),
            )
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn bytes(svc: &Service, d: &Delivery, path: Option<&str>) -> Vec<u8> {
    let (mut f, _, _, _) = svc.open(d, path).unwrap();
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).unwrap();
    bytes
}

#[tokio::test]
async fn local_file_versions_survive_cleanup_and_restart() {
    let (home, svc, s) = fixture();
    let ws = crate::wire::workspaces::Workspaces::new(svc.store.clone(), None, None, vec![]);
    let path = Path::new(&s.cwd).join("report.md");
    fs::write(&path, b"first").unwrap();
    let old = svc
        .receive(&s, path.to_str().unwrap(), Some("Report"), None, &ws)
        .await
        .unwrap();
    fs::write(&path, b"second").unwrap();
    let new = svc.receive(&s, "report.md", None, None, &ws).await.unwrap();
    assert_ne!(old.id, new.id);
    assert_eq!(bytes(&svc, &old, None), b"first");
    assert_eq!(bytes(&svc, &new, None), b"second");
    fs::remove_dir_all(&s.cwd).unwrap();
    let reloaded = Service::new(
        Store::open(home.path().join("test.sqlite3"), steve_store::NoPublisher).unwrap(),
        home.path().to_path_buf(),
        svc.base.clone(),
    );
    assert_eq!(
        bytes(
            &reloaded,
            &reloaded.get(&old.id, Some("default")).unwrap().unwrap(),
            None
        ),
        b"first"
    );
    assert!(reloaded.get(&old.id, Some("other")).unwrap().is_none());
    let meta = svc.metadata(&old, None);
    assert_eq!(meta["url"], svc.url(&old.id, &old.workspace_id));
    assert!(meta["created"].as_i64().unwrap() > 1_000_000_000_000);
    assert!(meta["entrypoint"].is_null());
    assert!(
        meta["access_warning"]
            .as_str()
            .unwrap()
            .contains("not been verified")
    );
    assert!(
        svc.reference(&s, &svc.url(&old.id, &old.workspace_id))
            .unwrap()
            .is_some()
    );
    let mut other = s.clone();
    other.workspace_id = "other".into();
    assert!(
        svc.reference(&other, &svc.url(&old.id, &old.workspace_id))
            .is_err()
    );
}

#[tokio::test]
async fn hub_owned_outputs_remain_available_when_the_bound_node_is_unavailable() {
    let (_home, svc, session) = fixture();
    let ws = crate::wire::workspaces::Workspaces::new(svc.store.clone(), None, None, vec![]);
    svc.store.transaction(|tx| {
        tx.execute("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES (?1,'remote','{\"node\":\"remote\",\"id\":\"w\"}',?1,'r','active',0)", [&session.id])?;
        Ok::<_, StoreError>(())
    }).unwrap();
    let durable = crate::wire::output_files::directory(&svc.home, "ST-1").unwrap();
    fs::create_dir_all(&durable).unwrap();
    let path = durable.join("hub-report.md");
    fs::write(&path, b"Hub owned output").unwrap();
    let d = svc.receive(&session, path.to_str().unwrap(), None, None, &ws).await.unwrap();
    assert_eq!(bytes(&svc, &d, None), b"Hub owned output");
}

#[tokio::test]
async fn local_delivery_rejects_links_folders_and_paths_outside_session() {
    let (_home, svc, s) = fixture();
    let ws = crate::wire::workspaces::Workspaces::new(svc.store.clone(), None, None, vec![]);
    fs::write(Path::new(&s.cwd).join("ok.md"), b"hello").unwrap();
    std::os::unix::fs::symlink("ok.md", Path::new(&s.cwd).join("link.md")).unwrap();
    fs::hard_link(
        Path::new(&s.cwd).join("ok.md"),
        Path::new(&s.cwd).join("hard.md"),
    )
    .unwrap();
    for name in ["link.md", "hard.md", "../test.sqlite3", ".", "missing.md"] {
        assert!(
            svc.receive(&s, name, None, None, &ws).await.is_err(),
            "{name}"
        );
    }
    let durable = crate::wire::output_files::directory(&svc.home, "ST-1").unwrap();
    fs::create_dir_all(durable.parent().unwrap()).unwrap();
    fs::write(Path::new(&s.cwd).join("clean.md"), b"one link").unwrap();
    std::os::unix::fs::symlink(&s.cwd, &durable).unwrap();
    assert!(
        svc.receive(
            &s,
            durable.join("clean.md").to_str().unwrap(),
            None,
            None,
            &ws
        )
        .await
        .is_err()
    );
    assert_eq!(
        svc.store
            .reader::<_, StoreError>(|c| Ok(c.query_row(
                "SELECT count(*) FROM deliveries",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .unwrap(),
        0
    );
}

#[test]
fn zip_resources_are_complete_and_immutable() {
    let (_home, svc, s) = fixture();
    let archive = zip(&[
        (
            "site/index.html",
            b"<link href=assets/site.css><a href=next.html>next</a>",
        ),
        ("site/assets/site.css", b"body{color:red}"),
        ("site/next.html", b"next"),
    ]);
    let d = svc
        .accept(
            &s,
            "Design",
            "design.zip",
            &archive,
            Some("site/index.html"),
        )
        .unwrap();
    assert_eq!(d.entrypoint.as_deref(), Some("site/index.html"));
    assert_eq!(bytes(&svc, &d, None), archive);
    assert_eq!(
        bytes(&svc, &d, Some("site/assets/site.css")),
        b"body{color:red}"
    );
    assert_eq!(bytes(&svc, &d, Some("site/next.html")), b"next");
    for path in [
        "../raw",
        "/site/index.html",
        "site/../index.html",
        "site/%2e%2e/a",
        "site\\index.html",
        "site/missing.html",
    ] {
        assert!(svc.open(&d, Some(path)).is_err(), "{path}");
    }
    assert!(svc.metadata(&d, None)["access_warning"].is_string());
}

#[test]
fn documents_images_pdf_and_generic_zip_have_the_existing_preview_shapes() {
    let (_home, svc, s) = fixture();
    for (name, mime, form, data) in [
        ("note.md", "text/plain", "text", b"# Note".as_slice()),
        (
            "picture.png",
            "image/png",
            "image",
            b"\x89PNG\r\n\x1a\n".as_slice(),
        ),
        (
            "report.pdf",
            "application/pdf",
            "pdf",
            b"%PDF-1.4\n".as_slice(),
        ),
        (
            "report.docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "office",
            b"document bytes".as_slice(),
        ),
    ] {
        let d = svc.accept(&s, name, name, data, None).unwrap();
        assert_eq!(d.mime, mime);
        assert_eq!(d.preview["type"], form);
        assert_eq!(bytes(&svc, &d, Some(name)), data);
        assert_eq!(bytes(&svc, &d, None), data);
    }
    let archive = zip(&[("note.txt", b"archive")]);
    let d = svc
        .accept(&s, "Archive", "archive.zip", &archive, None)
        .unwrap();
    assert!(d.entrypoint.is_none());
    let meta = svc.metadata(&d, None);
    assert_eq!(meta["content_url"], meta["download_url"]);
}

#[test]
fn warnings_use_the_configured_address_and_keep_static_html_restrictions() {
    let (_home, svc, s) = fixture();
    let text = svc.accept(&s, "Text", "note.md", b"note", None).unwrap();
    let html = svc
        .accept(&s, "HTML", "index.html", b"<p>ok</p>", None)
        .unwrap();
    let svg = svc.accept(&s, "SVG", "image.svg", b"<svg/>", None).unwrap();
    let bundle = svc
        .accept(
            &s,
            "Bundle",
            "design.zip",
            &zip(&[("index.html", b"ok")]),
            None,
        )
        .unwrap();
    for (base, local) in [
        ("http://127.0.0.1:8440", true),
        ("http://127.12.34.56", true),
        ("http://localhost", true),
        ("https://preview.localhost.", true),
        ("http://[::1]:8440", true),
        ("http://[::ffff:127.0.0.2]", true),
        ("http://0.0.0.0:8440", true),
        ("http://[::]:8440", true),
        ("http://192.168.1.2:8440", false),
        ("https://steve.example", false),
        ("https://localhost.example", false),
        ("https://127.0.0.1.example", false),
    ] {
        let mut configured = svc.clone();
        configured.base = base.into();
        for delivery in [&text, &html, &svg, &bundle] {
            for share in [None, Some("example-token")] {
                let meta = configured.metadata(delivery, share);
                let warning = meta["access_warning"].as_str().unwrap();
                assert_eq!(
                    warning.contains("loopback or local-only"),
                    local,
                    "{base}: {warning}"
                );
                assert_eq!(
                    warning.contains("static sandbox"),
                    delivery.id != text.id,
                    "{warning}"
                );
                if !local {
                    assert!(warning.contains("not been verified"));
                }
            }
        }
    }
}

#[test]
fn delivery_references_preserve_registered_urls_and_reject_forged_selectors() {
    let (_home, svc, s) = fixture();
    let d = svc
        .accept(&s, "Report", "report.md", b"report", None)
        .unwrap();
    let legacy = format!("{}/deliveries/{}", svc.base, d.id);
    assert_eq!(svc.reference(&s, &legacy).unwrap().unwrap().id, d.id);
    assert_eq!(
        svc.reference(&s, &d.registered_url).unwrap().unwrap().id,
        d.id
    );
    for suffix in [
        "?ws=wrong",
        "?ws=default&extra=1",
        "?ws=default&ws=foreign",
        "#fragment",
    ] {
        assert!(
            svc.reference(&s, &format!("{legacy}{suffix}")).is_err(),
            "{suffix}"
        );
    }
    let foreign = format!("https://outside.example/deliveries/{}?ws=default", d.id);
    assert!(svc.reference(&s, &foreign).unwrap().is_none());
    assert!(svc.reference(&s, &format!("{legacy}/raw")).is_err());
}

#[test]
fn malicious_or_incomplete_zip_never_creates_a_delivery() {
    let (_home, svc, s) = fixture();
    for path in [
        "../escape.html",
        "/absolute.html",
        "C:/drive.html",
        "a\\b.html",
        "a/./b.html",
        "a/%2e%2e/b.html",
    ] {
        assert!(
            svc.accept(&s, "bad", "bad.zip", &zip(&[(path, b"x")]), None)
                .is_err(),
            "{path}"
        );
    }
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .add_symlink(
            "link.html",
            "/etc/passwd",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
    assert!(
        svc.accept(
            &s,
            "bad",
            "bad.zip",
            &writer.finish().unwrap().into_inner(),
            None
        )
        .is_err()
    );
    assert!(
        svc.accept(
            &s,
            "bad",
            "bad.zip",
            &zip(&[("a", b"file"), ("a/b.html", b"child")]),
            None
        )
        .is_err()
    );
    let bomb = zip(&[("bomb.html", &vec![b'x'; 3 * 1024 * 1024])]);
    assert!(svc.accept(&s, "bad", "bad.zip", &bomb, None).is_err());
    let good = zip(&[("index.html", b"<p>ok</p>")]);
    let mut bad_crc = good.clone();
    let central = bad_crc.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    bad_crc[central + 16] ^= 1;
    assert!(svc.accept(&s, "bad", "bad.zip", &bad_crc, None).is_err());
    let mut duplicate = zip(&[("a.html", b"a"), ("b.html", b"b")]);
    for i in 0..duplicate.len() - 6 {
        if &duplicate[i..i + 6] == b"b.html" {
            duplicate[i] = b'a';
        }
    }
    assert!(svc.accept(&s, "bad", "bad.zip", &duplicate, None).is_err());
    assert!(
        svc.accept(&s, "bad", "bad.zip", &good[..good.len() - 10], None)
            .is_err()
    );
    assert!(
        svc.accept(&s, "bad", "bad.zip", &good, Some("missing.html"))
            .is_err()
    );
    assert!(
        svc.accept(&s, "bad", "bad.txt", b"ok", Some("index.html"))
            .is_err()
    );
    let too_many: Vec<_> = (0..1025).map(|i| format!("file-{i}")).collect();
    let ents: Vec<_> = too_many
        .iter()
        .map(|n| (n.as_str(), b"".as_slice()))
        .collect();
    assert!(svc.accept(&s, "bad", "bad.zip", &zip(&ents), None).is_err());
    assert_eq!(
        svc.store
            .reader::<_, StoreError>(|c| Ok(c.query_row(
                "SELECT count(*) FROM deliveries",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .unwrap(),
        0
    );
}
