mod sg42;
use serde_json::json;
use sg42::{Env, zip};
use steve_hub::deliveries::Publisher;
use steve_store::{NewTag, ProjectPatch};

#[tokio::test]
async fn item_patch_rejects_invalid_tags_before_any_write_and_orders_change_notes() {
    let e = Env::new(false).await;
    let project = e.project("new project", "dir");
    let kinds = e.store.tags().kinds("default").unwrap();
    let feat = kinds.iter().find(|t| t.name == "feat").unwrap();
    let bug = kinds.iter().find(|t| t.name == "bug").unwrap();
    let old = e
        .store
        .tags()
        .create(
            "default",
            &NewTag {
                name: "old label".into(),
                cat: "topic".into(),
                ..Default::default()
            },
            &e.user,
        )
        .unwrap();
    let new = e
        .store
        .tags()
        .create(
            "default",
            &NewTag {
                name: "new label".into(),
                cat: "topic".into(),
                ..Default::default()
            },
            &e.user,
        )
        .unwrap();
    e.store
        .tags()
        .set_item(&e.item.id, &[feat.id.clone(), old.id.clone()])
        .unwrap();
    e.patch_item(json!({"cwd":"previous-place","branch":"previous-branch"}));
    let path = format!("/items/{}", e.item.id);
    let before = e.store.timeline().list(&e.item.id).unwrap().len();
    let (s, _) = e
        .call(
            "PATCH",
            &path,
            Some(json!({"title":"must not stick","tags":[old.id]})),
        )
        .await;
    assert_eq!(s, 409);
    assert_eq!(
        e.store.items().get(&e.item.id).unwrap().unwrap().title,
        "original"
    );
    assert_eq!(e.store.timeline().list(&e.item.id).unwrap().len(), before);
    let input = json!({"title":"revised","due":"2030-01-01","project_id":project.id,"tags":[bug.id,new.id],"star":true,"priority":"高"});
    let (s, v) = e.call("PATCH", &path, Some(input.clone())).await;
    assert_eq!(s, 200, "{v}");
    assert!(v["cwd"].is_null() && v["branch"].is_null());
    assert_eq!(v["project_id"], project.id);
    let notes = e.store.timeline().list(&e.item.id).unwrap();
    let notes = &notes[before..];
    assert_eq!(
        notes.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(),
        [
            "标题改为「revised」",
            "截止时间设为 2030-01-01",
            "项目改为 new project，下一个会话起生效",
            "分类改为 bug（原来是 feat）",
            "添加标签：new label",
            "移除标签：old label",
        ]
    );
    assert_eq!(
        notes.iter().map(|x| x.kind.as_str()).collect::<Vec<_>>(),
        ["sys", "sys", "sys", "me", "me", "me"]
    );
    assert!(notes.iter().all(|x| x.actor == e.user));
    let n = before + notes.len();
    assert_eq!(e.call("PATCH", &path, Some(input)).await.0, 200);
    assert_eq!(
        e.store.timeline().list(&e.item.id).unwrap().len(),
        n,
        "repeating unchanged fields adds no note"
    );
    e.stop().await;
}

#[tokio::test]
async fn foreign_project_is_rejected_before_tag_mutation_or_timeline_notes() {
    let e = Env::new(false).await;
    let foreign = e
        .store
        .workspaces()
        .create(
            &steve_store::NewWorkspace {
                name: "Foreign".into(),
                ..Default::default()
            },
            &e.user,
        )
        .unwrap();
    let p = e
        .store
        .projects()
        .create(
            &foreign.id,
            &steve_store::NewProject {
                name: "Foreign project".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let before = e.store.tags().of_item(&e.item.id).unwrap();
    let n = e.store.timeline().list(&e.item.id).unwrap().len();
    let (s, _) = e
        .call(
            "PATCH",
            &format!("/items/{}", e.item.id),
            Some(json!({"project_id":p.id,"tags":[],"title":"hidden"})),
        )
        .await;
    assert_eq!(s, 404);
    assert_eq!(e.store.tags().of_item(&e.item.id).unwrap(), before);
    assert_eq!(e.store.timeline().list(&e.item.id).unwrap().len(), n);
    e.stop().await;
}

#[tokio::test]
async fn project_release_patch_preserves_script_rules_clear_semantics_and_validation_order() {
    let e = Env::new(false).await;
    for kind in ["git", "dir"] {
        let p = e.project(kind, kind);
        let path = format!("/projects/{}", p.id);
        let (s, _) = e
            .call("PATCH", &path, Some(json!({"done_when":"tag"})))
            .await;
        assert_eq!(s, if kind == "git" { 200 } else { 400 });
        let (s, v) = e
            .call(
                "PATCH",
                &path,
                Some(json!({"release":{"kind":"script","cmd":"/bin/true"}})),
            )
            .await;
        assert_eq!(s, 200, "{v}");
        assert_eq!(v["release"]["cmd"], "/bin/true");
        for bad in [
            json!({"done_when":"deployed"}),
            json!({"release":{"kind":"other","cmd":"x"}}),
            json!({"release":{"kind":"script","cmd":"x","done_when":"bad"}}),
        ] {
            assert_eq!(e.call("PATCH", &path, Some(bad)).await.0, 400);
            assert_eq!(
                e.store
                    .projects()
                    .get(&p.id)
                    .unwrap()
                    .unwrap()
                    .release
                    .unwrap()
                    .cmd,
                "/bin/true"
            );
        }
        assert_eq!(
            e.call("PATCH", &path, Some(json!({"done_when":"merge"})))
                .await
                .0,
            200,
            "existing scripts accept a completion rule on either project kind"
        );
        let (s, v) = e
            .call("PATCH", &path, Some(json!({"done_when":null})))
            .await;
        assert_eq!(s, 200);
        assert_eq!(v["release"]["cmd"], "/bin/true");
        assert!(v["release"]["done_when"].is_null());
        assert_eq!(
            e.call(
                "PATCH",
                &path,
                Some(json!({"release":{"kind":"script","cmd":"  "}}))
            )
            .await
            .0,
            200
        );
        assert!(
            e.store
                .projects()
                .get(&p.id)
                .unwrap()
                .unwrap()
                .release
                .is_none()
        );
        let(s,_)=e.call("PATCH",&path,Some(json!({"name":"must not stick","release":{"kind":"script","cmd":"x","done_when":"tag"}}))).await;
        assert_eq!(s, if kind == "git" { 200 } else { 400 });
        if kind == "dir" {
            assert_eq!(e.store.projects().get(&p.id).unwrap().unwrap().name, "dir");
        }
        assert_eq!(
            e.call("PATCH", &path, Some(json!({"release":null})))
                .await
                .0,
            200
        );
    }
    e.stop().await;
}

#[tokio::test]
async fn raw_output_ranges_headers_and_node_placement_preserve_the_same_public_contract() {
    for local in [false, true] {
        let e = Env::new(local).await;
        let p = e.project("outputs", "dir");
        let root = std::path::PathBuf::from(&p.path);
        e.patch_item(json!({"project_id":p.id,"cwd":p.path}));
        e.ws.refresh_roots().unwrap();
        for (name, mime, pdf) in [
            ("clip.mp4", "video/mp4", false),
            ("paper.pdf", "application/pdf", true),
            ("note.txt", "application/octet-stream", false),
        ] {
            let f = root.join(name);
            std::fs::write(&f, b"0123456789").unwrap();
            let a = e.artifact(name, &f);
            let path = format!("/items/{}/artifacts/{a}/raw", e.item.id);
            for (range, status, body, span) in [
                (None, 200, b"0123456789".as_slice(), None),
                (
                    Some("bytes=2-4"),
                    206,
                    b"234".as_slice(),
                    Some("bytes 2-4/10"),
                ),
                (
                    Some("bytes=-3"),
                    206,
                    b"789".as_slice(),
                    Some("bytes 7-9/10"),
                ),
                (Some("bytes=90-99"), 200, b"0123456789".as_slice(), None),
            ] {
                let (s, h, b) = e.raw("GET", &path, None, range).await;
                assert_eq!(s, status);
                assert_eq!(b, body);
                assert_eq!(h["content-type"], mime);
                assert_eq!(h["accept-ranges"], "bytes");
                assert_eq!(h["content-length"], body.len().to_string());
                assert_eq!(h.get("content-range").map(|v| v.to_str().unwrap()), span);
                assert_eq!(h.contains_key("content-security-policy"), !pdf);
                assert_eq!(h["cross-origin-resource-policy"], "same-origin");
                assert_eq!(h["cache-control"], "private, no-cache");
            }
            let (s, h, b) = e
                .raw("GET", &format!("{path}?download=1"), None, None)
                .await;
            assert_eq!(s, 200);
            assert_eq!(b, b"0123456789");
            assert_eq!(h["content-type"], "application/octet-stream");
            assert!(
                h["content-disposition"]
                    .to_str()
                    .unwrap()
                    .starts_with("attachment")
            );
            let (s, v) = e
                .call(
                    "GET",
                    &format!("/items/{}/artifacts/{a}/preview", e.item.id),
                    None,
                )
                .await;
            assert_eq!(s, 200, "{v}");
            assert_ne!(v["type"], "missing");
        }
        let dir = e.artifact("folder", &root);
        assert_eq!(
            e.raw(
                "GET",
                &format!("/items/{}/artifacts/{dir}/raw", e.item.id),
                None,
                None
            )
            .await
            .0,
            404
        );
        if local {
            // Without an item-owned cwd, placement follows the project device.
            e.patch_item(json!({"cwd":null}));
            e.store
                .projects()
                .patch(
                    &p.id,
                    &ProjectPatch {
                        device_id: Some("missing-device".into()),
                        path: Some("work/repo".into()),
                        ..Default::default()
                    },
                    &e.user,
                )
                .unwrap();
            assert_eq!(
                e.call(
                    "GET",
                    &format!("/items/{}/artifacts/{dir}/preview", e.item.id),
                    None
                )
                .await
                .0,
                503,
                "a missing Node never falls back to local disk"
            );
        }
        e.stop().await;
    }
}

#[tokio::test]
async fn published_versions_keep_bundle_preview_owner_scope_and_immutable_objects() {
    let e = Env::new(false).await;
    let session = e.session();
    let svc = e.st.deliveries.as_ref().unwrap();
    let bytes = zip(&[
        ("index.html", b"<script>run()</script>"),
        ("asset.txt", b"asset"),
    ]);
    let d = svc
        .publish(
            Publisher::Session(&session),
            "Page",
            "page.zip",
            &bytes,
            Some("index.html"),
            true,
        )
        .unwrap();
    assert_eq!(d.entrypoint.as_deref(), Some("index.html"));
    assert_eq!(d.preview["type"], "html");
    assert_eq!(d.preview["scripts"], true);
    assert!(d.interactive);
    assert_eq!(
        e.q::<i64>("SELECT COUNT(*) FROM json_each((SELECT manifest FROM deliveries LIMIT 1))"),
        2
    );
    let (mut file, _, mime, _) = svc.open(&d, Some("asset.txt")).unwrap();
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut out).unwrap();
    assert_eq!(out, b"asset");
    assert!(mime.starts_with("text/"));
    let user = svc
        .publish(
            Publisher::User {
                id: &e.user,
                workspace: "default",
            },
            "",
            "plain.txt",
            b"owned",
            None,
            false,
        )
        .unwrap();
    assert_eq!(user.title, "plain.txt");
    assert_eq!(user.owner_id.as_deref(), Some(e.user.as_str()));
    assert!(user.item_id.is_none());
    assert!(!user.interactive);
    let before = e.q::<i64>("SELECT COUNT(*) FROM deliveries");
    for (name, bytes, entry) in [
        ("../escape.txt", b"no".to_vec(), None),
        ("note.txt", b"no".to_vec(), Some("index.html")),
        ("bad.zip", zip(&[("../escape", b"no")]), None),
    ] {
        assert!(
            svc.publish(
                Publisher::Session(&session),
                "bad",
                name,
                &bytes,
                entry,
                false
            )
            .is_err()
        );
        assert_eq!(e.q::<i64>("SELECT COUNT(*) FROM deliveries"), before);
    }
    let mut stale = session.clone();
    stale.workspace_id = "foreign".into();
    assert!(
        svc.publish(
            Publisher::Session(&stale),
            "bad",
            "bad.txt",
            b"no",
            None,
            false
        )
        .unwrap_err()
        .contains("workspace changed")
    );
    e.stop().await;
}

#[tokio::test]
async fn received_file_versions_survive_source_removal_and_bound_sources_fail_closed() {
    let e = Env::new(true).await;
    let session = e.session();
    let svc = e.st.deliveries.as_ref().unwrap();
    let path = std::path::Path::new(&session.cwd).join("report.md");
    let first = vec![b'a'; 128 * 1024 + 7];
    std::fs::write(&path, &first).unwrap();
    let d = svc
        .receive_as(&session, "report.md", None, None, false, &e.ws)
        .await
        .unwrap();
    assert_eq!(d.size, first.len() as u64);
    assert_eq!(d.title, "report.md");
    std::fs::write(&path, b"second").unwrap();
    let next = svc
        .receive_as(&session, "report.md", Some("Revised"), None, true, &e.ws)
        .await
        .unwrap();
    assert_ne!(d.id, next.id);
    assert_eq!(next.title, "Revised");
    assert!(next.interactive);
    std::fs::remove_file(&path).unwrap();
    let (mut file, _, _, _) = svc.open(&d, None).unwrap();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes).unwrap();
    assert_eq!(bytes, first);
    let before = e.q::<i64>("SELECT COUNT(*) FROM deliveries");
    for path in [
        ".git/config",
        "node_modules/pkg",
        "target/output",
        "missing.md",
    ] {
        assert!(
            svc.receive_as(&session, path, None, None, false, &e.ws)
                .await
                .is_err()
        );
    }
    std::fs::write(&path, b"Hub placeholder must not be read").unwrap();
    e.store.transaction::<_,steve_store::StoreError>(|tx|{tx.execute("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES (?1,'sg42-node',?2,?1,'r','ready',1)",rusqlite::params![session.id,json!({"node":"sg42-node","id":"bound-workspace"}).to_string()])?;Ok(())}).unwrap();
    let error = svc
        .receive_as(&session, "report.md", None, None, false, &e.ws)
        .await
        .unwrap_err();
    assert!(error.contains("scoped Node execution"), "{error}");
    assert_eq!(e.q::<i64>("SELECT COUNT(*) FROM deliveries"), before);
    e.stop().await;
}
