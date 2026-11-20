//! Real YOLO engine for isolated outbound fixtures. There are no agent or model side effects.
use std::{path::Path, sync::Arc, time::Duration};

use steve_core::{Core, CoreConfig, error::Result, gateway, repo::Db, runtime::*};
use steve_modules::{Clock, Modules};

struct Inactive;
impl AgentRuntime for Inactive {
    fn prompt(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &[steve_core::repo::FileRef],
    ) -> Result<Prompted> {
        panic!("outbound recovery must not start an agent")
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
        panic!("outbound recovery must not answer a tool permission")
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
#[async_trait::async_trait]
impl gateway::ModelGateway for Inactive {
    async fn call(
        &self,
        _: &str,
        _: gateway::Tier,
        _: Duration,
        _: &gateway::ThinkTools,
    ) -> Result<gateway::Printed> {
        panic!("channel approvals use facts, never a model's opinion about human approval")
    }
}

/// Rebuild Core and YOLO memory over the Modules' live Store. Decision dispatch still uses
/// the actual channel handler; the durable writer remains shared while Modules is alive.
pub fn engine(modules: &Modules, home: &Path, clock: Arc<dyn Clock>) -> steve_core::yolo::Engine {
    let db = Db::new(modules.store.clone());
    let core = Core::new(
        db,
        Arc::new(Inactive),
        Arc::new(Inactive),
        CoreConfig::for_home(home),
    );
    core.attach_modules(modules.clone());
    let yolo = steve_core::yolo::Yolo::with_clock(core.store().clone(), None, home, clock);
    core.set_yolo(yolo.clone());
    steve_core::yolo::Engine::new(core, yolo)
}

pub fn enable(engine: &steve_core::yolo::Engine, full: bool) {
    engine
        .yolo()
        .put(
            steve_modules::testkit::WS,
            steve_modules::testkit::ME,
            steve_core::yolo::Switch {
                on: true,
                full,
                preview: false,
                include_open: true,
            },
        )
        .unwrap();
}
