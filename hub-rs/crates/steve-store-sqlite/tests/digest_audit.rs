#![allow(dead_code)]
pub struct Audit(pub steve_store::Store);
pub fn check(store: &steve_store::Store) {
    steve_store::testkit::digest::audit(store).unwrap();
}
impl Drop for Audit {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            check(&self.0);
        }
    }
}
