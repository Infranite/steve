//! Upgrade progress is cumulative SQLite VM work and ordered storage preparation phases.
use crate::signing::{atomic_json, json_file, owned_file};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use steve_store_sqlite::migrate::{self, Phase};

pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const PHASES: [&str; 4] = [
    "legacy_replay",
    "validation",
    "install_contract",
    "domain_migration",
];
fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::LegacyReplay => PHASES[0],
        Phase::Validation => PHASES[1],
        Phase::InstallContract => PHASES[2],
        Phase::DomainMigration => PHASES[3],
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    pub format: u32,
    pub upgrade_id: String,
    pub pid: u32,
    pub phase: String,
    pub domain: Option<String>,
    pub vm_steps: u64,
    pub complete: bool,
}
impl Progress {
    fn position(&self) -> Option<(usize, usize)> {
        if self.format != 2 {
            return None;
        }
        let phase = PHASES.iter().position(|p| *p == self.phase)?;
        let domain = match &self.domain {
            Some(domain) if phase == 3 => {
                migrate::CATALOG.iter().position(|s| s.domain == domain)? + 1
            }
            None => 0,
            _ => return None,
        };
        if self.complete && (phase != 3 || domain != migrate::CATALOG.len()) {
            return None;
        }
        Some((phase, domain))
    }
}
pub fn path(home: &Path, id: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(id)?;
    Ok(home.join("updates").join(format!("migration-{id}.json")))
}
pub fn read(home: &Path, id: &str) -> Option<Progress> {
    let path = path(home, id).ok()?;
    owned_file(&path).ok()?;
    let value = json_file(&path).ok()?;
    // Option's serde default would otherwise accept an omitted domain field,
    // unlike the Python consumer of this same progress protocol.
    value.get("domain")?;
    serde_json::from_value(value).ok()
}
/// The engine emits complete only after its final transaction commits. Stage/domain
/// boundaries are immediate; VM-only reports remain rate-limited to once per second.
pub struct Reporter {
    path: PathBuf,
    progress: Option<Progress>,
    written: Option<Instant>,
    id: String,
}
impl Reporter {
    pub fn new(home: &Path, id: &str) -> Result<Self> {
        Ok(Self {
            path: path(home, id)?,
            progress: None,
            written: None,
            id: id.into(),
        })
    }
    pub fn report(&mut self, value: migrate::Progress) -> Result<()> {
        let progress = Progress {
            format: 2,
            upgrade_id: self.id.clone(),
            pid: std::process::id(),
            phase: phase_name(value.phase).into(),
            domain: value.domain.map(str::to_owned),
            vm_steps: value.vm_steps,
            complete: value.complete,
        };
        let position = progress
            .position()
            .ok_or_else(|| anyhow::anyhow!("invalid storage progress phase/completion"))?;
        if let Some(old) = &self.progress {
            ensure!(
                !old.complete
                    && position >= old.position().unwrap()
                    && progress.vm_steps >= old.vm_steps,
                "regressing storage progress"
            );
        }
        let boundary = self.progress.as_ref().is_none_or(|p| {
            p.phase != progress.phase
                || p.domain != progress.domain
                || p.complete != progress.complete
        });
        self.progress = Some(progress);
        if boundary
            || self
                .written
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(1))
        {
            atomic_json(&self.path, &serde_json::to_value(&self.progress)?)?;
            self.written = Some(Instant::now());
        }
        Ok(())
    }
}
pub struct Budget {
    id: String,
    pid: u32,
    started: Instant,
    advanced: Instant,
    idle: Duration,
    total: Duration,
    seen: Option<Progress>,
}
impl Budget {
    pub fn new(id: &str, pid: u32, idle: Duration) -> Self {
        Self::with_limits(id, pid, idle, TOTAL_TIMEOUT, Instant::now())
    }
    fn with_limits(id: &str, pid: u32, idle: Duration, total: Duration, now: Instant) -> Self {
        Self {
            id: id.into(),
            pid,
            started: now,
            advanced: now,
            idle,
            total,
            seen: None,
        }
    }
    pub fn check(&mut self, progress: Option<Progress>) -> Result<()> {
        self.check_at(Instant::now(), progress)
    }
    fn check_at(&mut self, now: Instant, progress: Option<Progress>) -> Result<()> {
        ensure!(
            now.duration_since(self.started) < self.total,
            "candidate maintenance probe exceeded total timeout; last progress: {:?}",
            self.seen
        );
        ensure!(
            now.duration_since(self.advanced) < self.idle,
            "candidate did not become healthy: {}; last progress: {:?}",
            if self.seen.as_ref().is_some_and(|p| p.complete) {
                "migration completed; health verification timed out"
            } else {
                "no migration progress"
            },
            self.seen
        );
        if let Some(p) = progress {
            let Some(position) = p.position() else {
                return Ok(());
            };
            if p.upgrade_id != self.id || p.pid != self.pid {
                return Ok(());
            }
            if let Some(old) = &self.seen
                && (old.complete
                    || position < old.position().unwrap()
                    || p.vm_steps < old.vm_steps
                    || (position == old.position().unwrap()
                        && p.vm_steps == old.vm_steps
                        && !p.complete))
            {
                return Ok(());
            }
            self.advanced = now;
            self.seen = Some(p);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "c2d95ee0-5fde-4309-b98b-5b670e021040";
    fn progress(phase: &str, domain: Option<&str>, steps: u64, complete: bool) -> Progress {
        Progress {
            format: 2,
            upgrade_id: ID.into(),
            pid: 123,
            phase: phase.into(),
            domain: domain.map(str::to_owned),
            vm_steps: steps,
            complete,
        }
    }
    fn budget(now: Instant) -> Budget {
        Budget::with_limits(ID, 123, Duration::from_secs(60), TOTAL_TIMEOUT, now)
    }
    #[test]
    fn vm_progress_extends_idle_but_never_total_timeout() {
        let now = Instant::now();
        let mut b = budget(now);
        for second in (0..900).step_by(10) {
            b.check_at(
                now + Duration::from_secs(second),
                Some(progress("validation", None, second, false)),
            )
            .unwrap();
        }
        assert!(
            b.check_at(now + TOTAL_TIMEOUT, None)
                .unwrap_err()
                .to_string()
                .contains("total timeout")
        );
    }
    #[test]
    fn phases_do_not_signal_completion_and_regressions_do_not_extend_idle() {
        let now = Instant::now();
        let mut b = budget(now);
        for (n, phase) in PHASES.into_iter().enumerate() {
            b.check_at(
                now + Duration::from_secs(n as u64 * 50),
                Some(progress(phase, None, 10, false)),
            )
            .unwrap();
        }
        assert!(!b.seen.as_ref().unwrap().complete);
        b.check_at(
            now + Duration::from_secs(200),
            Some(progress("install_contract", None, 20, true)),
        )
        .unwrap();
        assert!(b.check_at(now + Duration::from_secs(210), None).is_err());
    }
    #[test]
    fn domain_progress_and_one_final_health_window() {
        let now = Instant::now();
        let mut b = budget(now);
        b.check_at(
            now,
            Some(progress("domain_migration", Some("core"), 10, false)),
        )
        .unwrap();
        b.check_at(
            now + Duration::from_secs(50),
            Some(progress("domain_migration", Some("comms"), 10, true)),
        )
        .unwrap();
        b.check_at(
            now + Duration::from_secs(100),
            Some(progress("domain_migration", Some("comms"), 20, false)),
        )
        .unwrap();
        assert!(
            b.check_at(now + Duration::from_secs(110), None)
                .unwrap_err()
                .to_string()
                .contains("migration completed")
        );
        assert!(
            budget(now)
                .check_at(
                    now + Duration::from_secs(61),
                    Some(progress("validation", None, 20, false))
                )
                .is_err()
        );
    }
    #[test]
    fn private_report_is_atomic_and_cannot_complete_before_final_phase() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let mut r = Reporter::new(home.path(), ID).unwrap();
        r.report(migrate::Progress {
            phase: Phase::Validation,
            domain: None,
            vm_steps: 10,
            complete: false,
        })
        .unwrap();
        assert_eq!(read(home.path(), ID).unwrap().phase, "validation");
        let report_path = path(home.path(), ID).unwrap();
        let original = json_file(&report_path).unwrap();
        let mut incomplete = original.clone();
        incomplete.as_object_mut().unwrap().remove("domain");
        atomic_json(&report_path, &incomplete).unwrap();
        assert!(read(home.path(), ID).is_none());
        atomic_json(&report_path, &original).unwrap();
        assert!(
            r.report(migrate::Progress {
                phase: Phase::InstallContract,
                domain: None,
                vm_steps: 20,
                complete: true
            })
            .is_err()
        );
        assert!(!read(home.path(), ID).unwrap().complete);
        assert_eq!(
            std::fs::metadata(path(home.path(), ID).unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
