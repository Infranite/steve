//! Upgrade-only migration progress. A SQLite VM counter is evidence of work; a timer, log write or
//! live PID is not. Old candidates may omit this optional contract and keep the ordinary timeout.
use crate::signing::{atomic_json, json_file, owned_file};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Progress {
    pub format: u32,
    pub upgrade_id: String,
    pub pid: u32,
    pub migration: u32,
    pub target: u32,
    pub vm_steps: u64,
    pub complete: bool,
}

pub fn path(home: &Path, id: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(id)?;
    Ok(home.join("updates").join(format!("migration-{id}.json")))
}

/// A missing, partial, corrupt or foreign record never extends a probe.
pub fn read(home: &Path, id: &str) -> Option<Progress> {
    let path = path(home, id).ok()?;
    owned_file(&path).ok()?;
    serde_json::from_value(json_file(&path).ok()?).ok()
}

/// Created only after the candidate validated its upgrade gate. Writes are atomic, private and at
/// most once a second inside a statement; migration boundaries and commit are written immediately.
pub struct Reporter {
    path: PathBuf,
    progress: Progress,
    written: Option<Instant>,
}
impl Reporter {
    pub fn new(home: &Path, id: &str) -> Result<Self> {
        Ok(Self {
            path: path(home, id)?,
            progress: Progress {
                format: 1,
                upgrade_id: id.to_owned(),
                pid: std::process::id(),
                migration: 0,
                target: 0,
                vm_steps: 0,
                complete: false,
            },
            written: None,
        })
    }

    pub fn report(
        &mut self,
        migration: u32,
        target: u32,
        vm_steps: u64,
        complete: bool,
    ) -> Result<()> {
        let boundary = migration != self.progress.migration || complete != self.progress.complete;
        self.progress.migration = migration;
        self.progress.target = target;
        self.progress.vm_steps = vm_steps;
        self.progress.complete = complete;
        if boundary
            || self
                .written
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(1))
        {
            self.written = Some(Instant::now());
            atomic_json(&self.path, &serde_json::to_value(&self.progress)?)?;
        }
        Ok(())
    }
}

/// The idle window is refreshed only by forward migration/VM progress from this child. A completed
/// migration gets one ordinary health window, then cannot keep an unhealthy candidate alive.
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
            id: id.to_owned(),
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
            "candidate maintenance probe exceeded its total timeout ({:?}); last migration progress: {:?}",
            self.total,
            self.seen
        );
        ensure!(
            now.duration_since(self.advanced) < self.idle,
            "candidate did not become healthy: {} {:?}; last migration progress: {:?}",
            if self.seen.as_ref().is_some_and(|p| p.complete) {
                "migration completed; health verification timed out after"
            } else {
                "no migration progress within"
            },
            self.idle,
            self.seen
        );
        if let Some(p) = progress {
            if p.format != 1
                || p.upgrade_id != self.id
                || p.pid != self.pid
                || p.migration == 0
                || p.migration > p.target
                || (p.complete && p.migration != p.target)
            {
                return Ok(());
            }
            if let Some(old) = &self.seen {
                if old.complete
                    || p.target != old.target
                    || p.migration < old.migration
                    || p.vm_steps < old.vm_steps
                    || (p.migration == old.migration && p.vm_steps == old.vm_steps && !p.complete)
                {
                    return Ok(());
                }
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
    fn progress(steps: u64, complete: bool) -> Progress {
        Progress {
            format: 1,
            upgrade_id: ID.into(),
            pid: 123,
            migration: 76,
            target: 76,
            vm_steps: steps,
            complete,
        }
    }
    fn budget(now: Instant) -> Budget {
        Budget::with_limits(ID, 123, Duration::from_secs(60), TOTAL_TIMEOUT, now)
    }
    #[test]
    fn long_statement_can_progress_past_sixty_seconds_but_never_past_total_limit() {
        let now = Instant::now();
        let mut b = budget(now);
        for seconds in (10..900).step_by(10) {
            b.check_at(
                now + Duration::from_secs(seconds),
                Some(progress(seconds, false)),
            )
            .unwrap();
        }
        assert!(
            b.check_at(now + TOTAL_TIMEOUT, Some(progress(900, false)))
                .unwrap_err()
                .to_string()
                .contains("total timeout")
        );
    }
    #[test]
    fn repeated_invalid_and_regressing_records_do_not_extend_the_idle_window() {
        let now = Instant::now();
        let mut invalid = vec![None, Some(progress(10, false)), Some(progress(9, false))];
        let mut foreign = progress(20, false);
        foreign.pid += 1;
        invalid.push(Some(foreign));
        let mut foreign = progress(20, false);
        foreign.upgrade_id = uuid::Uuid::new_v4().to_string();
        invalid.push(Some(foreign));
        let mut changed = progress(20, false);
        changed.target += 1;
        invalid.push(Some(changed));
        let mut backward = progress(20, false);
        backward.migration -= 1;
        invalid.push(Some(backward));
        let mut malformed = progress(20, false);
        malformed.format = 2;
        invalid.push(Some(malformed));
        for p in invalid {
            let mut b = budget(now);
            b.check_at(now, Some(progress(10, false))).unwrap();
            b.check_at(now + Duration::from_secs(50), p).unwrap();
            assert!(b.check_at(now + Duration::from_secs(60), None).is_err());
        }
    }
    #[test]
    fn finishing_only_grants_one_health_window_and_cannot_revive_an_expired_probe() {
        let now = Instant::now();
        let mut b = budget(now);
        b.check_at(now, Some(progress(10, false))).unwrap();
        b.check_at(now + Duration::from_secs(50), Some(progress(10, true)))
            .unwrap();
        b.check_at(now + Duration::from_secs(100), Some(progress(20, false)))
            .unwrap();
        assert!(
            b.check_at(now + Duration::from_secs(110), Some(progress(30, true)))
                .unwrap_err()
                .to_string()
                .contains("migration completed")
        );
        assert!(
            budget(now)
                .check_at(now + Duration::from_secs(61), Some(progress(30, false)))
                .is_err()
        );
    }
    #[test]
    fn private_atomic_report_round_trip_and_corrupt_or_old_file_handling() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        assert!(read(home.path(), ID).is_none());
        let mut reporter = Reporter::new(home.path(), ID).unwrap();
        reporter.report(76, 76, 10, false).unwrap();
        let file = path(home.path(), ID).unwrap();
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(read(home.path(), ID).unwrap().vm_steps, 10);
        std::fs::write(&file, b"{partial").unwrap();
        assert!(read(home.path(), ID).is_none());
        assert!(path(home.path(), "../foreign").is_err());
    }
}
