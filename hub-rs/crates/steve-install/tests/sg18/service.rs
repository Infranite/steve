// Included in service's existing cfg(test) module; both managers run through its inert runner.
fn sg18_mutations(calls: &[Vec<String>]) -> Vec<String> {
    calls
        .iter()
        .filter_map(|call| {
            call.iter()
                .find(|arg| {
                    matches!(
                        arg.as_str(),
                        "disable" | "daemon-reload" | "bootout" | "stop" | "kill"
                    )
                })
                .cloned()
        })
        .collect()
}
#[test]
fn sg18_uninstall_platform_receipts_and_idempotence_table() {
    for system in [System::Darwin, System::Linux] {
        for component in ["hub", "node"] {
            let f = Fixture::new();
            if component == "hub" {
                f.maintenance_tools(system);
            }
            let (mut s, m) = f.service(system, component);
            s.dispatch("install", Some(&f.release), 0).unwrap();
            let definition = fs::read(&s.config).unwrap();
            let record = fs::read(&s.record_path).unwrap();
            m.lock().unwrap().calls.clear();
            let result = s.dispatch("uninstall", None, 0).unwrap();
            assert_eq!(result["installed"], false);
            assert_eq!(result["changed"], true);
            assert_eq!(result["started"], false);
            assert_eq!(result["home_preserved"], true);
            let archive = PathBuf::from(result["archive"].as_str().unwrap());
            assert!(archive.starts_with(f.home.join("services/retired")));
            assert_eq!(fs::read(archive.join("definition")).unwrap(), definition);
            assert_eq!(fs::read(archive.join("record.json")).unwrap(), record);
            assert_eq!(s.record["uninstall"]["phase"], "complete");
            assert_eq!(s.record["uninstalled"], true);
            assert!(!s.config.exists());
            assert_eq!(
                sg18_mutations(&m.lock().unwrap().calls),
                if system == System::Linux {
                    vec!["daemon-reload", "disable", "daemon-reload"]
                } else {
                    vec![]
                },
                "{system:?}/{component}"
            );
            let saved = fs::read(&s.record_path).unwrap();
            m.lock().unwrap().calls.clear();
            let second = s.dispatch("uninstall", None, 0).unwrap();
            assert_eq!(second["changed"], false);
            assert_eq!(second["home_preserved"], true);
            assert_eq!(second["archive"], s.record["uninstall"]["archive"]);
            assert_eq!(fs::read(&s.record_path).unwrap(), saved);
            assert!(sg18_mutations(&m.lock().unwrap().calls).is_empty());
            assert_eq!(
                fs::read_dir(f.home.join("services/retired"))
                    .unwrap()
                    .count(),
                1
            );
            f.preserved();
        }
    }
}

#[test]
fn sg18_uninstall_unregistered_mismatched_running_and_unknown_table() {
    for system in [System::Darwin, System::Linux] {
        for condition in ["unregistered", "component", "label", "running", "unknown"] {
            let f = Fixture::new();
            let (mut s, m) = f.service(system, "node");
            if condition != "unregistered" {
                s.dispatch("install", Some(&f.release), 0).unwrap();
            }
            if matches!(condition, "component" | "label") {
                let mut record = s.record.clone();
                record[condition] = json!("foreign");
                atomic(&s.record_path, &json_bytes(&record).unwrap()).unwrap();
            }
            {
                let mut manager = m.lock().unwrap();
                manager.calls.clear();
                manager.running = condition == "running";
                manager.loaded = condition == "running";
                manager.unknown = condition == "unknown";
            }
            let before = if s.config.exists() {
                Some(fs::read(&s.config).unwrap())
            } else {
                None
            };
            let error = s.dispatch("uninstall", None, 0).unwrap_err();
            if matches!(condition, "running" | "unknown") {
                assert!(
                    error
                        .to_string()
                        .contains("service is running, loaded, or its state is unknown"),
                    "{error:#}"
                );
            } else {
                assert!(
                    format!("{error:#}").contains(s.record_path.to_str().unwrap()),
                    "{condition}: {error:#}"
                );
                assert!(m.lock().unwrap().calls.is_empty());
            }
            assert!(sg18_mutations(&m.lock().unwrap().calls).is_empty());
            assert_eq!(
                if s.config.exists() {
                    Some(fs::read(&s.config).unwrap())
                } else {
                    None
                },
                before
            );
            assert!(!f.home.join("services/retired").exists());
            f.preserved();
        }
    }
}

#[test]
fn sg18_uninstall_interruption_keeps_one_original_archive_and_resumes() {
    for point in ["linux-disable", "linux-reload", "darwin-pending-read"] {
        let system = if point.starts_with("linux") {
            System::Linux
        } else {
            System::Darwin
        };
        let f = Fixture::new();
        let (mut s, m) = f.service(system, "node");
        s.dispatch("install", Some(&f.release), 0).unwrap();
        let definition = fs::read(&s.config).unwrap();
        let original_record = fs::read(&s.record_path).unwrap();
        if system == System::Linux {
            let mut manager = m.lock().unwrap();
            manager.failure = Some(
                if point == "linux-disable" {
                    "disable"
                } else {
                    "daemon-reload"
                }
                .into(),
            );
            manager.fail_after_removal = point == "linux-reload";
        } else {
            let manager = m.clone();
            let record = s.record_path.clone();
            let fired = Arc::new(Mutex::new(false));
            s.runner = Arc::new(move |args| {
                let output = manager.lock().unwrap().run(args)?;
                if args.iter().any(|a| a == "print")
                    && !*fired.lock().unwrap()
                    && object(&record)
                        .ok()
                        .is_some_and(|r| r["uninstall"]["phase"] == "pending")
                {
                    *fired.lock().unwrap() = true;
                    return Ok(Output {
                        code: 1,
                        stdout: String::new(),
                        stderr: "access denied after archive".into(),
                    });
                }
                Ok(output)
            });
        }
        assert!(s.dispatch("uninstall", None, 0).is_err());
        let pending = object(&s.record_path).unwrap();
        assert_eq!(pending["uninstall"]["phase"], "pending");
        let archive = f
            .home
            .join("services/retired")
            .join(pending["uninstall"]["archive"].as_str().unwrap());
        assert_eq!(fs::read(archive.join("definition")).unwrap(), definition);
        assert_eq!(
            fs::read(archive.join("record.json")).unwrap(),
            original_record
        );
        assert_eq!(s.config.exists(), point != "linux-reload");
        {
            let mut manager = m.lock().unwrap();
            manager.failure = None;
            manager.calls.clear();
        }
        let mut resumed =
            Service::with_runner("node", &f.home, None, system, &f.root, s.runner.clone()).unwrap();
        assert_eq!(
            resumed.dispatch("uninstall", None, 0).unwrap()["changed"],
            true
        );
        assert_eq!(resumed.record["uninstall"]["phase"], "complete");
        assert_eq!(
            fs::read_dir(f.home.join("services/retired"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(fs::read(archive.join("definition")).unwrap(), definition);
        assert_eq!(
            fs::read(archive.join("record.json")).unwrap(),
            original_record
        );
        if point == "linux-reload" {
            assert!(!sg18_mutations(&m.lock().unwrap().calls).contains(&"disable".to_string()));
        }
        f.preserved();
    }
}

#[test]
fn sg18_uninstall_tampered_archive_never_removes_the_live_definition() {
    for change in ["definition", "record.json"] {
        let f = Fixture::new();
        let (mut s, m) = f.service(System::Linux, "node");
        s.dispatch("install", Some(&f.release), 0).unwrap();
        let definition = fs::read(&s.config).unwrap();
        m.lock().unwrap().failure = Some("disable".into());
        assert!(s.dispatch("uninstall", None, 0).is_err());
        let archive = f
            .home
            .join("services/retired")
            .join(s.record["uninstall"]["archive"].as_str().unwrap());
        fs::write(archive.join(change), b"sg18 tampered archive").unwrap();
        {
            let mut manager = m.lock().unwrap();
            manager.failure = None;
            manager.calls.clear();
        }
        let mut resumed = Service::with_runner(
            "node",
            &f.home,
            None,
            System::Linux,
            &f.root,
            s.runner.clone(),
        )
        .unwrap();
        let error = resumed.dispatch("uninstall", None, 0).unwrap_err();
        assert!(
            error.to_string().contains("backup changed"),
            "{change}: {error:#}"
        );
        assert_eq!(fs::read(&s.config).unwrap(), definition);
        assert!(
            sg18_mutations(&m.lock().unwrap().calls)
                .iter()
                .all(|c| c == "daemon-reload")
        );
        f.preserved();
    }
}
