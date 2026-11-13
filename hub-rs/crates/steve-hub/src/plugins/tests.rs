use super::*;

/// A directory library with one plugin, `echo`, at `version`; returns the directory.
fn library(root: &Path, version: &str) -> PathBuf {
    let dir = root.join("library");
    std::fs::create_dir_all(dir.join("dist")).unwrap();
    let archive = format!("dist/echo-{version}.tar.gz");
    let sha = install::tests::archive(
        &dir.join(&archive),
        &[
            ("bin/echo", 0o755, b"#!/bin/sh\ncat\n"),
            ("VERSION", 0o644, version.as_bytes()),
        ],
    );
    let mut artifacts = serde_json::Map::new();
    artifacts.insert(
        env!("STEVE_BUILD_TARGET").into(),
        json!({"url": archive, "sha256": sha}),
    );
    let index = json!({
        "store": 1,
        "name": {"en": "Test library", "zh-CN": "测试库"},
        "plugins": [{
            "id": "echo", "name": "Echo", "summary": "Says it back", "kind": "channel",
            "version": version, "contract": 1, "entry": "bin/echo",
            "config": [{"key": "address", "label": "Address"},
                       {"key": "token", "label": "Token", "secret": true},
                       {"key": "note", "label": "Note", "required": false}],
            "artifacts": artifacts,
        }, {
            "id": "later", "name": "Later", "summary": "From a newer Steve", "kind": "tool",
            "version": "1.0.0", "contract": 1, "entry": "bin/later",
        }]
    });
    std::fs::write(
        dir.join(index::INDEX_FILE),
        serde_json::to_vec(&index).unwrap(),
    )
    .unwrap();
    dir
}

fn who() -> Caller {
    Caller {
        ws: "default".into(),
        me: "u1".into(),
    }
}

#[tokio::test]
async fn a_new_installation_lists_the_official_library_without_reading_it() {
    let home = tempfile::tempdir().unwrap();
    let p = Plugins::new(home.path(), None).unwrap();
    let v = p.list(&who(), true).unwrap();
    let stores = v["stores"].as_array().unwrap();
    assert_eq!(stores.len(), 1, "{v}");
    assert_eq!(stores[0]["id"], "infranite-steve-extensions");
    assert_eq!(stores[0]["address"], OFFICIAL_LIBRARY);
    assert_eq!(stores[0]["official"], true);
    assert!(stores[0]["checked"].is_null() && stores[0]["plugins"].as_array().unwrap().is_empty());
    assert_eq!(v["target"], env!("STEVE_BUILD_TARGET"));
    // nothing was written: the list is still the default one
    assert!(!home.path().join("plugins").exists());

    let none = Plugins::new(home.path(), Some("off".into())).unwrap();
    assert!(none.stores().unwrap().is_empty());
    let other = Plugins::new(home.path(), Some("someone/plugins".into())).unwrap();
    assert_eq!(
        other.stores().unwrap()[0].address,
        "https://github.com/someone/plugins"
    );
}

#[tokio::test]
async fn a_library_is_added_installed_from_upgraded_and_removed() {
    let home = tempfile::tempdir().unwrap();
    let dir = library(home.path(), "0.1.0");
    let p = Plugins::new(&home.path().join("steve"), Some("off".into())).unwrap();

    // an address without an index is not added
    let e = p
        .add_store(home.path().to_str().unwrap())
        .await
        .unwrap_err();
    assert!(matches!(e, Refused::Rule(400, _)), "{e:?}");
    assert!(p.stores().unwrap().is_empty());

    let rec = p.add_store(dir.to_str().unwrap()).await.unwrap();
    assert_eq!(rec.id, "library");
    let twice = p
        .add_store(&format!("{}/", dir.display()))
        .await
        .unwrap_err();
    assert!(matches!(twice, Refused::Rule(409, _)), "{twice:?}");

    // listed: what can be installed here, and what needs a newer Steve
    let v = p.list(&who(), false).unwrap();
    let s = &v["stores"][0];
    assert!(
        s.get("address").is_none(),
        "a path on the Hub's machine is the owner's to see: {s}"
    );
    assert_eq!(s["name"]["zh-CN"], "测试库");
    let (echo, later) = (&s["plugins"][0], &s["plugins"][1]);
    assert_eq!(
        (&echo["available"], &echo["unsupported"]),
        (&json!(true), &Value::Null)
    );
    assert!(echo["installed"].is_null());
    assert_eq!(
        (&later["available"], &later["unsupported"]),
        (&json!(false), &json!("kind"))
    );

    assert!(matches!(
        p.install("library", "later").await.unwrap_err(),
        Refused::Rule(400, _)
    ));
    assert!(matches!(
        p.install("library", "nope").await.unwrap_err(),
        Refused::NotFound(_)
    ));
    assert!(matches!(
        p.install("nope", "echo").await.unwrap_err(),
        Refused::NotFound(_)
    ));

    let done = p.install("library", "echo").await.unwrap();
    assert_eq!(done.plugin.version, "0.1.0");
    let root = home.path().join("steve/plugins/installed/library/echo");
    assert_eq!(std::fs::read(root.join("0.1.0/VERSION")).unwrap(), b"0.1.0");

    // the library publishes 0.2.0: an update is offered once the index is read again
    library(home.path(), "0.2.0");
    assert_eq!(
        p.list(&who(), true).unwrap()["stores"][0]["plugins"][0]["update"],
        false
    );
    p.refresh("library").await.unwrap();
    let v = p.list(&who(), true).unwrap();
    let echo = &v["stores"][0]["plugins"][0];
    assert_eq!(
        (&echo["version"], &echo["installed"]["version"]),
        (&json!("0.2.0"), &json!("0.1.0"))
    );
    assert_eq!(echo["update"], true);
    p.install("library", "echo").await.unwrap();
    assert!(root.join("0.2.0/bin/echo").is_file() && !root.join("0.1.0").exists());
    assert_eq!(
        p.installed("library", "echo").unwrap().plugin.version,
        "0.2.0"
    );

    // an index that stops being readable is recorded; what was read before stays
    std::fs::write(dir.join(index::INDEX_FILE), b"{").unwrap();
    let rec = p.refresh("library").await.unwrap();
    assert!(rec.error.is_some() && rec.index.is_some());
    let v = p.list(&who(), false).unwrap();
    assert_eq!(v["stores"][0]["failed"], true);
    assert!(v["stores"][0].get("error").is_none());

    // a library with something installed stays; uninstalling frees it
    let e = p.remove_store("library").await.unwrap_err();
    assert!(matches!(e, Refused::Rule(409, _)), "{e:?}");
    p.uninstall("library", "echo").await.unwrap();
    assert!(!root.exists());
    assert!(matches!(
        p.uninstall("library", "echo").await.unwrap_err(),
        Refused::NotFound(_)
    ));
    p.remove_store("library").await.unwrap();
    // the owner emptied the list: the first library does not come back by itself
    assert!(
        Plugins::new(&home.path().join("steve"), None)
            .unwrap()
            .stores()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn settings_are_the_ones_the_plugin_asks_for() {
    let field = |key: &str, required: bool| Field {
        key: key.into(),
        label: index::Text::One(key.into()),
        hint: None,
        secret: false,
        required,
    };
    let fields = [field("address", true), field("note", false)];
    let values = |kv: &[(&str, &str)]| -> BTreeMap<String, String> {
        kv.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    let ok =
        Plugins::check_values(&fields, values(&[("address", " wss://x "), ("note", "")])).unwrap();
    assert_eq!(ok, values(&[("address", "wss://x")]));
    for bad in [
        values(&[("note", "n")]),
        values(&[("address", "a"), ("extra", "x")]),
    ] {
        assert!(matches!(
            Plugins::check_values(&fields, bad),
            Err(Refused::Rule(400, _))
        ));
    }
}

#[tokio::test]
async fn a_theme_plugin_is_installed_for_the_web_client_and_never_runs() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("themes");
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = json!({"id": "ocean", "name": {"en": "Ocean"}, "kind": "accent",
        "tokens": {"light": {"primary": "#0b6bc0"}, "dark": {"primary": "#4aa3f0"}}});
    let good = install::tests::archive(
        &dir.join("ocean.tar.gz"),
        &[("theme.json", 0o644, manifest.to_string().as_bytes())],
    );
    let bad = install::tests::archive(&dir.join("bad.tar.gz"), &[("theme.json", 0o644, b"[1]")]);
    let theme = |id: &str, url: &str, sha: &str| {
        json!({"id": id, "name": "Ocean", "summary": "A blue accent", "kind": "theme",
            "version": "1.0.0", "contract": 1, "entry": "theme.json",
            "artifacts": {"any": {"url": url, "sha256": sha}}})
    };
    let index = json!({"store": 1, "name": "Themes", "plugins": [
        theme("ocean", "ocean.tar.gz", &good), theme("broken", "bad.tar.gz", &bad)]});
    std::fs::write(dir.join(index::INDEX_FILE), index.to_string()).unwrap();
    let p = Plugins::new(&home.path().join("steve"), Some("off".into())).unwrap();
    p.add_store(dir.to_str().unwrap()).await.unwrap();

    // an archive for any machine fits this one
    let v = p.list(&who(), true).unwrap();
    assert_eq!(v["stores"][0]["plugins"][0]["available"], true, "{v}");
    assert!(p.themes().unwrap().is_empty());

    p.install("themes", "ocean").await.unwrap();
    let file = home
        .path()
        .join("steve/plugins/installed/themes/ocean/1.0.0/theme.json");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "a theme is not made executable");
    }
    let themes = p.themes().unwrap();
    assert_eq!(themes.len(), 1);
    assert_eq!(
        (&themes[0]["plugin"], &themes[0]["manifest"]),
        (&json!("ocean"), &manifest)
    );

    // a manifest that is not an object with an id is not installed
    let e = p.install("themes", "broken").await.unwrap_err();
    assert!(matches!(e, Refused::Rule(400, _)), "{e:?}");
    assert!(p.installed("themes", "broken").is_none());
}
