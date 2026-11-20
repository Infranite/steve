use super::*;

pub(super) fn recover_pending(
    home: &Path,
    input: &mut dyn Prompts,
    inspect: &impl Fn(&Path) -> Result<Runtime>,
) -> Result<()> {
    let pending = Pending::load(home)?;
    crate::ui::info(&t!(
        "install.reconfigure.pending_found",
        pending = pending.directory.display()
    ));
    if !confirm(input, &t!("install.reconfigure.recover_question"))? {
        crate::ui::info(&t!("install.reconfigure.recover_cancelled"));
        return Ok(());
    }
    let _locks = locks(home)?;
    // Also recheck the actual installation, rather than trusting paths in a journal alone.
    inspect(home)?;
    let (archive, committed) = recover(home)?;
    crate::ui::info(&t!(
        "install.reconfigure.recovered",
        outcome = if committed {
            t!("install.reconfigure.recovered_committed")
        } else {
            t!("install.reconfigure.recovered_reverted")
        },
        archive = archive.display()
    ));
    Ok(())
}

pub(super) fn confirm_changes(
    snapshot: &Snapshot,
    changes: &BTreeMap<String, String>,
    input: &mut dyn Prompts,
) -> Result<bool> {
    crate::ui::info(&t!("install.reconfigure.about_to_save"));
    for (name, new) in changes {
        if snapshot.document.values().get(name) != Some(new) {
            crate::ui::info(&format!("  {name} = {new}"));
        }
    }
    if changes.is_empty() {
        crate::ui::info(&t!("install.reconfigure.sync_receipt_only"));
    }
    crate::ui::info(&t!("install.reconfigure.secret_kept"));
    confirm(input, &t!("install.reconfigure.save_question"))
}

pub(super) fn save_locked(
    snapshot: &Snapshot,
    candidate: &[u8],
    candidate_receipt: &[u8],
    inspect: &impl Fn(&Path) -> Result<Runtime>,
    check: &impl Fn(&Runtime, &Path, &BTreeMap<String, String>) -> Result<()>,
) -> Result<PathBuf> {
    snapshot.verify()?;
    let home = &snapshot.home;
    let verified_runtime = inspect(home)?;
    ensure!(
        verified_runtime.release == snapshot.runtime.release
            && verified_runtime.label == snapshot.runtime.label
            && verified_runtime.guards.iter().all(|(path, guard)| snapshot
                .runtime
                .guards
                .get(path)
                == Some(guard)),
        "{}",
        t!("install.reconfigure.state_changed_asking")
    );
    let values = EnvDocument::parse(candidate)?;
    check(&snapshot.runtime, home, values.values())?;
    snapshot.verify()?;
    let pending = Pending::prepare(snapshot, candidate, candidate_receipt)?;
    match apply(pending, |_| {
        #[cfg(test)]
        super::tests::sg18_after_write()?;
        Ok(())
    }) {
        Ok(archive) => Ok(archive),
        Err(error) => match recover(home) {
            Ok((archive, false)) => Err(error.context(t!(
                "install.reconfigure.save_reverted",
                archive = archive.display()
            ))),
            Ok((archive, true)) => Err(error.context(t!(
                "install.reconfigure.committed_archived",
                archive = archive.display()
            ))),
            Err(recovery) => Err(error.context(t!(
                "install.reconfigure.write_unfinished",
                pending = pending_path(home).display(),
                recovery = format!("{:#}", recovery)
            ))),
        },
    }
}
