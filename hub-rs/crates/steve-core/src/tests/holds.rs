//! Holds have reasons: a release drain and an upgrade's maintenance hold new turns independently.

use crate::AgentRuntime;
use crate::occupancy::Hold;
use crate::testkit::Kit;

#[test]
fn lifting_maintenance_keeps_a_release_hold() {
    let k = Kit::new();
    k.core.tx().set_hold(Hold::Release, true);
    k.core.maintenance_hold(true).unwrap();
    k.core.maintenance_hold(false).unwrap();
    assert!(k.rt.is_holding(), "the release still holds");
    assert!(k.core.tx().held_by(Hold::Release));
    k.core.tx().end_hold().unwrap();
    assert!(!k.rt.is_holding());
}

#[test]
fn ending_a_release_hold_keeps_maintenance() {
    let k = Kit::new();
    k.core.maintenance_hold(true).unwrap();
    k.core.tx().set_hold(Hold::Release, true);
    k.core.tx().end_hold().unwrap();
    assert!(k.rt.is_holding(), "maintenance still holds");
    assert!(k.core.tx().holding());
    k.core.maintenance_hold(false).unwrap();
    assert!(!k.rt.is_holding());
    assert!(!k.core.tx().holding());
}

#[test]
fn a_release_drain_during_maintenance_still_takes_its_own_hold() {
    let k = Kit::new();
    k.core.maintenance_hold(true).unwrap();
    // maintenance alone is not a release hold: no release status to show
    assert!(!k.core.tx().held_by(Hold::Release));
    k.core.tx().set_hold(Hold::Release, true);
    assert!(k.core.tx().held_by(Hold::Release));
    k.core.maintenance_hold(false).unwrap();
    assert!(k.rt.is_holding());
}

#[test]
fn queued_prompts_wait_until_every_hold_is_gone() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let sid = k.session(&i, "idle");
    k.core.maintenance_hold(true).unwrap();
    k.core.tx().set_hold(Hold::Release, true);
    k.rt.prompt(&sid, "等一下", "me", &[]).unwrap();
    k.core.maintenance_hold(false).unwrap();
    assert!(k.rt.prompts_of(&sid).is_empty(), "the release still holds");
    k.core.tx().end_hold().unwrap();
    assert!(k.wait(|| k.rt.prompts_of(&sid) == vec!["等一下"]));
}
