//! Audit the pending projection when each migrated-store integration fixture is torn down.
pub struct TestEnv(steve_modules::testkit::TestEnv);
impl TestEnv {
    pub fn new(register: impl FnOnce(&mut steve_modules::Registry)) -> Self {
        Self(steve_modules::testkit::TestEnv::new(register))
    }
}

impl std::ops::Deref for TestEnv {
    type Target = steve_modules::testkit::TestEnv;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            steve_store::testkit::digest::audit(self.0.store()).unwrap();
        }
    }
}
