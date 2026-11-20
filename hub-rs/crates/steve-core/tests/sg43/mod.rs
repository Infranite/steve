//! A temporary Store/Core with no executable backend; only public construction APIs.
use std::{path::Path, sync::Arc, time::Duration};
use steve_core::{
    Core, CoreConfig, Result,
    gateway::{ModelGateway, Printed, ThinkTools, Tier},
    repo::{Db, FileRef},
    runtime::{AgentRuntime, Answered, Flushed, Prompted, Released, Running, Stop},
};
struct InertRuntime;
impl AgentRuntime for InertRuntime {
    fn prompt(&self, _: &str, _: &str, _: &str, _: &[FileRef]) -> Result<Prompted> {
        panic!("unexpected SG43 agent work")
    }
    fn running(&self, _: &str) -> Option<Running> {
        None
    }
    fn stop(&self, _: &Running) -> Result<Stop> {
        Ok(Stop::NotRunning)
    }
    fn is_busy(&self, _: &str) -> bool {
        false
    }
    fn release(&self, _: &str) -> Result<Released> {
        Ok(Released::Gone)
    }
    fn answer_permission(&self, _: &str, _: Option<&str>) -> Result<Answered> {
        Ok(Answered::Gone)
    }
    fn hold(&self, _: bool) {}
    fn is_holding(&self) -> bool {
        false
    }
    fn running_turns(&self) -> usize {
        0
    }
    fn flush(&self, _: &str, _: Option<(&str, &str)>) -> Flushed {
        Flushed::Empty
    }
}
struct NoModel;
#[async_trait::async_trait]
impl ModelGateway for NoModel {
    async fn call(&self, _: &str, _: Tier, _: Duration, _: &ThinkTools) -> Result<Printed> {
        panic!("unexpected SG43 model work")
    }
}
pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub core: Core,
}
impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = CoreConfig::for_home(dir.path());
        cfg.resume_delay = Duration::from_secs(3600);
        cfg.gh = "/usr/bin/false".into();
        std::fs::create_dir_all(&cfg.data_dir).unwrap();
        std::fs::create_dir_all(&cfg.scratch).unwrap();
        std::fs::create_dir_all(&cfg.worktrees).unwrap();
        let raw =
            steve_store::Store::open(cfg.data_dir.join("steve.sqlite3"), steve_store::NoPublisher)
                .unwrap();
        let core = Core::new(Db::new(raw), Arc::new(InertRuntime), Arc::new(NoModel), cfg);
        core.seed().unwrap();
        Self { dir, core }
    }
    pub fn db(&self) -> &Db {
        self.core.store()
    }
    pub fn home(&self) -> &Path {
        self.dir.path()
    }
}
