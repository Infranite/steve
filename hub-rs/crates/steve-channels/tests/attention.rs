//! Personal channel indicators respect visibility/read state, not Steve's participation mode.
#[path = "support/digest_env.rs"]
mod digest_env;
use digest_env::TestEnv;
use steve_channels::conversations;
use steve_modules::testkit::{ME, OTHER, WS, WS2};
use steve_store::base::Viewer;
use steve_store::testkit::channels::seed;

fn setup() -> TestEnv {
    let e = TestEnv::new(|_| {});
    seed::spaces::attention_accounts(e.store()).unwrap();
    e
}
fn add(e: &TestEnv, id: &str, kind: &str, connection: &str) {
    seed::spaces::seed_attention_space(e.store(), id, kind, connection).unwrap();
}
fn list(e: &TestEnv, user: &str) -> Vec<steve_store::attention::Entry> {
    e.store()
        .read(|c| conversations::attention(c, WS, &Viewer::bare(WS, user), 200))
        .unwrap()
}
#[test]
fn channel_attention_is_personal_visible_enabled_and_quiet_is_not_mute() {
    let e = setup();
    add(&e, "dm", "dm", "bot");
    add(&e, "group", "group", "bot");
    add(&e, "private", "dm", "own");
    add(&e, "closed", "dm", "bot");
    add(&e, "blocked", "group", "bot");
    add(&e, "left", "group", "bot");
    seed::spaces::attention_preferences(e.store()).unwrap();
    let mine = list(&e, ME);
    assert_eq!(
        mine.len(),
        3,
        "quiet and role muted still surface unread; closed/blocked/left do not: {mine:?}"
    );
    assert!(mine.iter().any(|x| x.id == "chan:dm" && x.notify));
    assert!(mine.iter().any(|x| x.id == "chan:private" && x.notify));
    assert!(mine.iter().any(|x| x.id == "chan:group" && !x.notify));
    assert!(mine.iter().all(|x| x.group_key.is_none()));
    let other = list(&e, OTHER);
    assert_eq!(
        other.len(),
        3,
        "attention is limited to shared or owned connections; closed state remains personal"
    );
    assert!(other.iter().all(|x| x.id != "chan:private"));
    assert!(other.iter().any(|x| x.id == "chan:closed"));
    assert!(
        e.store()
            .read(|c| conversations::attention(c, WS2, &Viewer::bare(WS2, OTHER), 200))
            .is_err()
    );
    e.store()
        .write(|tx| {
            steve_channels::conv_record::mark_read(
                tx,
                WS,
                &Viewer::bare(WS, ME),
                "chan:dm",
                100,
                200,
            )
        })
        .unwrap();
    assert!(!list(&e, ME).iter().any(|x| x.id == "chan:dm"));
    assert!(list(&e, OTHER).iter().any(|x| x.id == "chan:dm"));
    seed::connections::pause_attention_bot(e.store()).unwrap();
    assert_eq!(
        list(&e, ME).len(),
        1,
        "disabled bot channels disappear, personal DM remains"
    );
    seed::connections::revoke_attention_owner(e.store()).unwrap();
    assert!(list(&e, ME).is_empty());
}

#[test]
fn channels_use_actual_live_incoming_visible_ids_not_unread_count_or_rowid() {
    let e = setup();
    add(&e, "dm", "dm", "bot");
    seed::messages::seed_attention_history(e.store()).unwrap();
    assert_eq!(list(&e, ME)[0].event_id, "message_dm");
    seed::messages::seed_live_attention_message(e.store()).unwrap();
    let a = list(&e, ME);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].event_id, "new-live");
    assert!(
        e.store()
            .read(|c| conversations::attention(
                c,
                WS,
                &Viewer::bare(WS, ME),
                steve_channels::conv_record::UNREAD_HORIZON_MS + 200
            ))
            .unwrap()
            .is_empty()
    );
}
