// Included only in reconfigure's existing cfg(test) module; reuses its original private fixture.
thread_local! {
    static SG18_WRITE_HOOK: std::cell::RefCell<Option<Box<dyn FnMut()->Result<()>>>> = std::cell::RefCell::new(None);
}
struct Sg18HookReset;
impl Drop for Sg18HookReset {
    fn drop(&mut self) {
        SG18_WRITE_HOOK.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}
fn sg18_with_hook<T>(hook: impl FnMut() -> Result<()> + 'static, work: impl FnOnce() -> T) -> T {
    SG18_WRITE_HOOK.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Box::new(hook));
    });
    let _reset = Sg18HookReset;
    work()
}
pub(super) fn sg18_after_write() -> Result<()> {
    SG18_WRITE_HOOK.with(|slot| match slot.borrow_mut().as_mut() {
        Some(hook) => hook(),
        None => Ok(()),
    })
}

fn sg18_env(f: &Fixture, values: BTreeMap<String, String>) {
    let doc = EnvDocument::parse(&fs::read(f.home.join("steve.env")).unwrap()).unwrap();
    fs::write(f.home.join("steve.env"), doc.update(&values).unwrap()).unwrap();
}
fn sg18_archives(f: &Fixture) -> Vec<PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(f.home.join(TRANSACTIONS))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap() != "pending")
        .collect();
    paths.sort();
    paths
}

#[test]
fn sg18_snapshot_oidc_and_vault_validation_table_preserves_all_bytes() {
    for case in [
        "issuer-only",
        "client-only",
        "relative-secret",
        "dir-unset",
        "relative-dir",
        "missing-dir",
        "inside-dir",
        "ancestor-dir",
        "file-missing",
        "unsupported",
        "valid-dir",
        "oidc-disabled",
    ] {
        let f = Fixture::new();
        let mut changes = BTreeMap::new();
        let expected = match case {
            "issuer-only" => {
                changes.insert("STEVE_OIDC_CLIENT_ID".into(), String::new());
                Some(t!(
                    "install.reconfigure.oidc.field_missing",
                    name = "STEVE_OIDC_CLIENT_ID"
                ))
            }
            "client-only" => {
                changes.insert("STEVE_OIDC_ISSUER".into(), String::new());
                Some(t!(
                    "install.reconfigure.oidc.field_missing",
                    name = "STEVE_OIDC_ISSUER"
                ))
            }
            "relative-secret" => {
                changes.insert(
                    "STEVE_OIDC_CLIENT_SECRET_FILE".into(),
                    "relative-secret".into(),
                );
                Some(t!("install.reconfigure.oidc.secret_path_relative"))
            }
            "unsupported" => {
                changes.insert("STEVE_VAULT_KEYSTORE".into(), "auto".into());
                Some(t!("install.reconfigure.vault.backend_unsupported"))
            }
            "file-missing" => {
                fs::remove_dir(f.home.join("vault-keys")).unwrap();
                Some(t!("install.reconfigure.vault.keys_missing"))
            }
            "oidc-disabled" => {
                for key in [
                    "STEVE_OIDC_ISSUER",
                    "STEVE_OIDC_CLIENT_ID",
                    "STEVE_OIDC_CLIENT_SECRET_FILE",
                ] {
                    changes.insert(key.into(), String::new());
                }
                None
            }
            _ => {
                changes.insert("STEVE_VAULT_KEYSTORE".into(), "dir".into());
                let path = match case {
                    "dir-unset" => None,
                    "relative-dir" => Some(PathBuf::from("relative-keys")),
                    "missing-dir" => Some(f._root.path().join("missing")),
                    "inside-dir" => Some(f.home.join("vault-keys")),
                    "ancestor-dir" => {
                        fs::set_permissions(f._root.path(), fs::Permissions::from_mode(0o700))
                            .unwrap();
                        Some(fs::canonicalize(f._root.path()).unwrap())
                    }
                    _ => {
                        let p = f._root.path().join("outside-keys");
                        private_dir(&p).unwrap();
                        Some(fs::canonicalize(p).unwrap())
                    }
                };
                if let Some(path) = path {
                    changes.insert("STEVE_VAULT_KEY_DIR".into(), path.display().to_string());
                }
                match case {
                    "dir-unset" => Some(t!("install.reconfigure.vault.dir_unset")),
                    "relative-dir" => Some(t!("install.reconfigure.vault.dir_relative")),
                    "missing-dir" => Some(t!("install.reconfigure.vault.dir_missing")),
                    "inside-dir" | "ancestor-dir" => {
                        Some(t!("install.reconfigure.vault.dir_inside_home"))
                    }
                    _ => None,
                }
            }
        };
        sg18_env(&f, changes);
        let before = tree(&f.home);
        let result = Snapshot::read(&f.home, &fixture_inspect);
        if let Some(expected) = expected {
            let error = result.err().expect(case);
            assert_eq!(error.to_string(), expected, "{case}: {error:#}");
        } else {
            result.unwrap().verify().unwrap();
        }
        assert_eq!(
            tree(&f.home),
            before,
            "{case} must not create keys or rewrite configuration"
        );
    }
}

#[test]
fn sg18_snapshot_protected_file_guards_and_path_rejection_table() {
    for case in ["parent", "absolute", "dot-component", "digest", "symlink"] {
        let f = Fixture::new();
        let receipt_path = f.home.join("setup.json");
        let mut record = read_json(&receipt_path).unwrap();
        let expected = match case {
            "parent" | "absolute" | "dot-component" => {
                let name = match case {
                    "parent" => "../escape".into(),
                    "absolute" => f.home.join("absolute").display().to_string(),
                    _ => "secrets/../escape".into(),
                };
                record["files"][name] = json!("0".repeat(64));
                t!("install.reconfigure.receipt.path_invalid")
            }
            "digest" => {
                record["files"]["secrets/oidc-client-secret"] = json!("0".repeat(64));
                t!(
                    "install.reconfigure.protected_file_modified",
                    name = "secrets/oidc-client-secret"
                )
            }
            _ => {
                let path = f.home.join("secrets/oidc-client-secret");
                let target = f.home.join("secrets/fixture-copy");
                fs::rename(&path, &target).unwrap();
                symlink(&target, &path).unwrap();
                String::new()
            }
        };
        fs::write(&receipt_path, json_bytes(&record).unwrap()).unwrap();
        let before = tree(&f.home);
        let error = Snapshot::read(&f.home, &fixture_inspect).err().expect(case);
        if case == "symlink" {
            assert!(format!("{error:#}").contains("oidc-client-secret"));
        } else {
            assert_eq!(error.to_string(), expected, "{case}: {error:#}");
        }
        assert_eq!(tree(&f.home), before);
    }
}

#[test]
fn sg18_pending_cancel_revert_and_committed_recovery_never_start_a_new_configuration() {
    for case in ["cancel", "reverted", "committed"] {
        let f = Fixture::new();
        let before = f.snapshot();
        let pending = f.pending();
        if case == "committed" {
            replace_if(
                &f.home.join("steve.env"),
                &pending.before_env,
                &pending.after_env,
            )
            .unwrap();
            replace_if(
                &f.home.join("setup.json"),
                &pending.before_receipt,
                &pending.after_receipt,
            )
            .unwrap();
            let mut journal = pending.journal;
            journal.phase = "committed".into();
            fs::write(
                pending.directory.join("journal.json"),
                serde_json::to_vec_pretty(&journal).unwrap(),
            )
            .unwrap();
        } else {
            assert!(apply(pending, |_| bail!("sg18 interrupted env write")).is_err());
        }
        let interrupted = tree(&f.home);
        let mut answers = Answers::new(&[if case == "cancel" { "" } else { "y" }]);
        run_with(&f.options(), &mut answers, fixture_inspect, |_, _, _| {
            panic!("pending recovery must not preflight new changes")
        })
        .unwrap();
        assert_eq!(answers.calls, 1);
        if case == "cancel" {
            assert_eq!(tree(&f.home), interrupted);
            assert!(exists(&pending_path(&f.home)));
        } else {
            assert!(!exists(&pending_path(&f.home)));
            let archives = sg18_archives(&f);
            assert_eq!(archives.len(), 1);
            assert_eq!(
                read_json(&archives[0].join("journal.json")).unwrap()["phase"],
                if case == "committed" {
                    "committed"
                } else {
                    "rolled_back"
                }
            );
            if case == "reverted" {
                before.verify().unwrap();
            } else {
                assert_ne!(fs::read(f.home.join("steve.env")).unwrap(), before.env);
                f.snapshot().verify().unwrap();
            }
        }
    }
}

#[test]
fn sg18_run_with_apply_failures_reach_all_three_real_recovery_messages() {
    for case in ["reverted", "committed", "unfinished"] {
        let f = Fixture::new();
        let before = f.snapshot();
        let home = f.home.clone();
        let mut writes = 0;
        let error = sg18_with_hook(
            move || {
                writes += 1;
                if case == "committed" && writes < 2 {
                    return Ok(());
                }
                if case == "committed" {
                    let pending = Pending::load(&home)?;
                    let mut journal = pending.journal;
                    journal.phase = "committed".into();
                    fs::write(
                        pending.directory.join("journal.json"),
                        serde_json::to_vec_pretty(&journal)?,
                    )?;
                }
                if case == "unfinished" {
                    fs::write(
                        home.join("signing/identity.json"),
                        b"sg18 external identity",
                    )?;
                }
                bail!("sg18 injected write failure")
            },
            || f.run(&mut Answers::new(&["", "https://new.example", "", "", "y"])),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("sg18 injected write failure"));
        let expected = if case == "unfinished" {
            assert!(exists(&pending_path(&f.home)));
            let recovery = recover(&f.home).unwrap_err();
            t!(
                "install.reconfigure.write_unfinished",
                pending = pending_path(&f.home).display(),
                recovery = format!("{recovery:#}")
            )
        } else {
            let archives = sg18_archives(&f);
            assert_eq!(archives.len(), 1);
            if case == "committed" {
                f.snapshot().verify().unwrap();
                t!(
                    "install.reconfigure.committed_archived",
                    archive = archives[0].display()
                )
            } else {
                before.verify().unwrap();
                t!(
                    "install.reconfigure.save_reverted",
                    archive = archives[0].display()
                )
            }
        };
        assert_eq!(error.to_string(), expected, "{case}");
        sg18_after_write().unwrap();
    }
}

#[test]
fn sg18_hook_drop_resets_after_panic_on_the_same_thread() {
    let result = std::panic::catch_unwind(|| {
        sg18_with_hook(
            || panic!("sg18 hook panic"),
            || {
                let _ = sg18_after_write();
            },
        )
    });
    assert!(result.is_err());
    sg18_after_write().unwrap();
    assert_eq!(
        sg18_with_hook(|| Ok(()), || sg18_after_write()).is_ok(),
        true
    );
}

#[test]
fn sg18_external_changes_after_locks_are_rejected_without_a_pending_write() {
    for target in ["steve.env", "setup.json", "signing/identity.json"] {
        let f = Fixture::new();
        let initial = f.snapshot();
        let calls = std::cell::Cell::new(0);
        let checks = std::cell::Cell::new(0);
        let path = f.home.join(target);
        let error = run_with(
            &f.options(),
            &mut Answers::new(&["", "https://new.example", "", "", "y"]),
            |home| {
                calls.set(calls.get() + 1);
                if calls.get() == 2 {
                    assert!(
                        locks(home).is_err(),
                        "external edit is after the setup/release locks"
                    );
                    fs::write(&path, b"sg18 external change")?;
                }
                fixture_inspect(home)
            },
            |_, _, _| {
                checks.set(checks.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(calls.get(), 2);
        assert!(!exists(&pending_path(&f.home)));
        assert_eq!(fs::read(&path).unwrap(), b"sg18 external change");
        assert_eq!(checks.get(), usize::from(target != "signing/identity.json"));
        assert_eq!(
            error.to_string(),
            if target == "signing/identity.json" {
                t!("install.reconfigure.state_changed_asking")
            } else {
                t!("install.reconfigure.changed_while_asking")
            }
        );
        if target != "steve.env" {
            assert_eq!(fs::read(f.home.join("steve.env")).unwrap(), initial.env);
        }
        if target != "setup.json" {
            assert_eq!(
                fs::read(f.home.join("setup.json")).unwrap(),
                initial.receipt
            );
        }
        drop(locks(&f.home).unwrap());
    }
}
