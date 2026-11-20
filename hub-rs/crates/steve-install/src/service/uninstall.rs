use super::*;

struct UninstallState {
    present: bool,
    definition: Option<Vec<u8>>,
    old_record: Option<Vec<u8>>,
    effective: Option<String>,
    pending: Value,
}

impl Service {
    pub(super) fn uninstall(&mut self) -> Result<Value> {
        let mut state = self.uninstall_snapshot()?;
        if !state.present && state.pending["phase"] != "pending" {
            return Ok(
                json!({"installed":false,"changed":false,"started":false,"home_preserved":true,"archive":state.pending["archive"]}),
            );
        }
        let archive = self.archive_uninstall(&mut state)?;
        self.remove_uninstall_definition(state.definition, state.effective)?;
        self.complete_uninstall(state.pending, &archive)
    }

    fn uninstall_snapshot(&self) -> Result<UninstallState> {
        self.require_registration("uninstall")?;
        let present = exists(&self.config);
        let definition = if present {
            Some(regular(&self.config)?)
        } else {
            None
        };
        if let Some(bytes) = &definition
            && self.system == System::Darwin
        {
            let value: Value = plist::from_bytes(bytes)?;
            ensure!(
                value["Label"] == self.label,
                "service definition label differs from the exact owned label"
            );
        }
        let old_record = if exists(&self.record_path) {
            Some(regular(&self.record_path)?)
        } else {
            None
        };
        ensure!(
            self.record.as_object().is_some_and(|o| o.is_empty())
                || (self.record["component"] == self.component
                    && self.record["label"] == self.label),
            "service record does not identify this component and label"
        );
        self.stopped()?;
        if present && self.system == System::Linux {
            self.systemctl(&["daemon-reload"], true)?;
            self.stopped()?;
        }
        let effective = if present && self.system == System::Linux {
            Some(self.isolated_unit(definition.as_ref().unwrap())?)
        } else {
            None
        };
        let pending = self.record.get("uninstall").cloned().unwrap_or(json!({}));
        ensure!(
            pending.is_object(),
            "uninstall receipt is invalid; inspect the retained service record"
        );
        Ok(UninstallState {
            present,
            definition,
            old_record,
            effective,
            pending,
        })
    }

    fn archive_uninstall(&mut self, state: &mut UninstallState) -> Result<PathBuf> {
        let services = self.home.join("services");
        directory(&services)?;
        let retired = services.join("retired");
        directory(&retired)?;
        let archive;
        if state.pending["phase"] == "pending" {
            archive = self.resume_uninstall_archive(
                &retired,
                &state.pending,
                state.definition.as_deref(),
            )?;
        } else {
            let definition = state
                .definition
                .as_ref()
                .context("uninstall definition disappeared")?;
            archive = retired.join(format!("{}-{}", self.component, Uuid::new_v4().simple()));
            directory(&archive)?;
            atomic(&archive.join("definition"), definition)?;
            ensure!(
                regular(&archive.join("definition"))? == *definition,
                "service definition backup did not verify"
            );
            if let Some(bytes) = &state.old_record {
                atomic(&archive.join("record.json"), bytes)?;
                ensure!(
                    regular(&archive.join("record.json"))? == *bytes,
                    "service record backup did not verify"
                );
            }
            state.pending = json!({"phase":"pending","archive":archive.file_name().unwrap().to_str().unwrap(),"config":text(&self.config)?,
                "definition_sha256":digest(definition),"record_sha256":state.old_record.as_ref().map(|bytes|digest(bytes))});
            self.record["component"] = json!(self.component);
            self.record["label"] = json!(self.label);
            self.record["uninstall"] = state.pending.clone();
            self.record["uninstalled"] = json!(false);
            atomic(&self.record_path, &json_bytes(&self.record)?)?;
        }
        Ok(archive)
    }

    fn complete_uninstall(&mut self, mut pending: Value, archive: &Path) -> Result<Value> {
        pending["phase"] = json!("complete");
        self.record["uninstall"] = pending;
        self.record["uninstalled"] = json!(true);
        let bytes = json_bytes(&self.record)?;
        atomic(&self.record_path, &bytes)?;
        ensure!(
            regular(&self.record_path)? == bytes,
            "uninstall receipt did not verify"
        );
        Ok(
            json!({"installed":false,"changed":true,"started":false,"home_preserved":true,"archive":text(&archive)?}),
        )
    }
}
