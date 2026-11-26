//! Repeated-ignore rule suggestion (#213).
//!
//! When the owner ignores similar messages in the same space [`IGNORE_SUGGEST_THRESHOLD`] times
//! within [`IGNORE_SUGGEST_WINDOW_DAYS`] days, Steve proposes a one-click "record only from now on"
//! (`以后只记录`) rule for that space (`DecisionKind::Propose`, `subject_type = "space"`).

use serde_json::{Value, json};
use steve_store::base::DecisionKind;
use steve_store::channel_message_repo::outbound as message_repo;
use steve_store::{DecisionRows, NewDecision, W};

use crate::ChannelResult;
use crate::spaces::{self, Attention, Mode, ReplyAsYou, SUBJECT_SPACE};

/// Number of times similar messages must be ignored before suggesting "record only".
pub const IGNORE_SUGGEST_THRESHOLD: usize = 3;
/// Rolling lookback window (days) for counting ignored similar messages.
pub const IGNORE_SUGGEST_WINDOW_DAYS: i64 = 7;
/// Rolling lookback window in milliseconds.
pub const IGNORE_SUGGEST_WINDOW_MS: i64 = IGNORE_SUGGEST_WINDOW_DAYS * 24 * 60 * 60 * 1000;
/// Options on the `propose` rule-suggestion decision (`subject_type = "space"`).
pub const RULE_SUGGEST_OPTIONS: [&str; 2] = ["record_only", "keep"];
/// Metadata kind stored in `decisions.result.kind` for ignore-driven rule suggestions.
pub const SUGGESTION_KIND_IGNORE_RECORD_ONLY: &str = "ignore_record_only";

/// Deterministic grouping of inbound messages into similar categories within a space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageCategory {
    /// Messages sent by a bot account.
    Bot,
    /// Direct messages from a human counterpart.
    Dm,
    /// Group messages that @-mention the speaking account.
    Mention,
    /// Other group messages (e.g. keyword or rule-triggered).
    Group,
}

impl MessageCategory {
    pub fn from_fact(fact: &message_repo::MessageCategoryFact) -> Self {
        if fact.sender_is_bot {
            Self::Bot
        } else if fact.space_kind == "dm" {
            Self::Dm
        } else if fact.is_mention {
            Self::Mention
        } else {
            Self::Group
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::Bot => "bot",
            Self::Dm => "dm",
            Self::Mention => "mention",
            Self::Group => "group",
        }
    }
}

fn is_ignore_resolution(kind: &str, result_json: Option<&str>) -> Option<i64> {
    let val = result_json.and_then(|raw| serde_json::from_str::<Value>(raw).ok())?;
    let choice = val.get("choice").and_then(Value::as_str)?;
    let ignored = (kind == "approve" && choice == "no") || (kind == "ask" && choice == "done");
    if !ignored {
        return None;
    }
    Some(val.get("ignored_at").and_then(Value::as_i64).unwrap_or(0))
}

fn last_suggestion_resolved_at(
    w: &mut W<'_>,
    ws: &str,
    space_id: &str,
) -> ChannelResult<Option<i64>> {
    let rows = message_repo::resolved_space_suggestions(w, ws, space_id)?;
    let max_at = rows
        .into_iter()
        .map(|(resolved, result_json)| {
            result_json
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                .and_then(|v| v.get("resolved_at").and_then(Value::as_i64))
                .unwrap_or(resolved)
        })
        .max();
    Ok(max_at)
}

/// Evaluates whether the current ignore (about to be committed as `current_decision_id` at `now`)
/// reaches [`IGNORE_SUGGEST_THRESHOLD`] similar ignores within [`IGNORE_SUGGEST_WINDOW_DAYS`] days
/// and, if so, opens a `propose` rule-suggestion decision on `space_id`.
pub fn on_message_ignored(
    w: &mut W<'_>,
    ws: &str,
    space_id: &str,
    message_id: &str,
    current_decision_id: &str,
    now: i64,
) -> ChannelResult<Option<String>> {
    let Some(space) = steve_store::channel_spaces::rows::get_in_workspace(w, ws, space_id)? else {
        return Ok(None);
    };
    if space.connection_id.is_empty() {
        return Ok(None);
    }
    let set = match spaces::settings(w, space_id) {
        Ok(set) => set,
        Err(steve_modules::ApiError::NotFound(_)) => return Ok(None),
        Err(err) => return Err(err),
    };
    let voice_mode = crate::bindings::connection_account(w, ws, &space.connection_id)
        .ok()
        .map(|(_, mode)| mode);
    let already_record_only = set.muted
        || set.attention >= Attention::Quiet
        || (voice_mode == Some(Mode::Own) && set.reply_as_you != ReplyAsYou::Draft);
    if already_record_only {
        return Ok(None);
    }
    if DecisionRows(w)
        .open_for(ws, SUBJECT_SPACE, space_id)?
        .is_some()
    {
        return Ok(None);
    }
    let Some(fact) = message_repo::message_category_fact(w, ws, message_id)? else {
        return Ok(None);
    };
    let category = MessageCategory::from_fact(&fact);
    let window_start = now.saturating_sub(IGNORE_SUGGEST_WINDOW_MS);
    let since = match last_suggestion_resolved_at(w, ws, space_id)? {
        Some(last) => window_start.max(last),
        None => window_start,
    };

    let recent = message_repo::recent_resolved_in_space(w, ws, space_id)?;
    let prior_same_category = recent
        .into_iter()
        .filter(|row| row.decision_id != current_decision_id)
        .filter(|row| MessageCategory::from_fact(&row.category) == category)
        .filter(|row| {
            is_ignore_resolution(&row.kind, row.result_json.as_deref()).is_some_and(|at| {
                let effective = if at > 0 { at } else { row.resolved };
                effective > since && effective <= now
            })
        })
        .count();
    let total = prior_same_category + 1;
    if total < IGNORE_SUGGEST_THRESHOLD {
        return Ok(None);
    }

    let title = spaces::display_title(w, &space)?;
    let settled = steve_store::decision_repo::settled_count(
        w,
        ws,
        SUBJECT_SPACE,
        space_id,
        DecisionKind::Propose.code(),
    )?;
    let d = DecisionRows(w).open(&NewDecision {
        workspace_id: ws.into(),
        kind: DecisionKind::Propose.code().into(),
        subject_type: SUBJECT_SPACE.into(),
        subject_id: space_id.into(),
        item_id: None,
        session_id: None,
        question: format!(
            "「{title}」里同类消息 {IGNORE_SUGGEST_WINDOW_DAYS} 天内已忽略 {total} 次，以后只记录吗？"
        ),
        draft: None,
        options: Some(RULE_SUGGEST_OPTIONS.map(String::from).to_vec()),
        requested_by: "steve".into(),
        question_template: None,
        source: Some(steve_store::DecisionSource::Revision {
            revision: format!("ignore_rule:{}:{settled}", category.code()),
        }),
        wait: None,
    })?;
    DecisionRows(w).set_result(
        &d.id,
        &json!({
            "kind": SUGGESTION_KIND_IGNORE_RECORD_ONLY,
            "category": category.code(),
            "ignored_count": total,
            "window_days": IGNORE_SUGGEST_WINDOW_DAYS,
            "suggested_at": now,
        }),
    )?;
    steve_store::audit_repo::record(
        w,
        ws,
        "steve",
        "space.rule_suggested",
        space_id,
        &json!({
            "decision": d.id,
            "category": category.code(),
            "ignored_count": total,
            "window_days": IGNORE_SUGGEST_WINDOW_DAYS,
        }),
    )?;
    metrics::counter!(
        "steve_channel_ignore_rule_suggestion_total",
        "category" => category.code()
    )
    .increment(1);
    Ok(Some(d.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use steve_flow::testkit::FakeCore;
    use steve_flow::{FlowSetup, LinearExecutor};
    use steve_modules::Clock;
    use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
    use steve_modules::decisions;
    use steve_modules::testkit::{ME, TestEnv, WS, caller};
    use steve_modules::vault::Vault;
    use steve_modules::vault::keystore::FileKeyStore;
    use steve_store::base::Viewer;

    use crate::channel::{InboundMessage, Mention, SpaceKind};
    use crate::fake::channel::FakeChannel;
    use crate::outbound::{OutLink, Outbox};
    use crate::respond::SUBJECT_MESSAGE;
    use crate::{Channels, Replies, space_decisions, spaces};

    struct Harness {
        env: TestEnv,
        _keys: tempfile::TempDir,
        ch: Channels,
        me: String,
    }

    fn harness() -> Harness {
        let core = Arc::new(FakeCore::default());
        let flow_setup = FlowSetup::new(core.clone());
        let link = OutLink::new();
        let reg_link = link.clone();
        let env = TestEnv::new(|r| {
            flow_setup.register(r);
            connections::register(r);
            r.decision(SUBJECT_MESSAGE, reg_link.handler());
            space_decisions::register(r);
        });
        let start = env.clock.now();
        link.bind_clock(env.clock.clone());
        let keys = tempfile::tempdir().unwrap();
        let vault = Vault::new(
            env.store().clone(),
            Arc::new(FileKeyStore::open(keys.path()).unwrap()),
        );
        let fake = Arc::new(FakeChannel::new());
        let conns = Connections::builder(
            env.modules.clone(),
            vault,
            ConnectionsConfig::new(Vec::new(), true, |_| None),
        )
        .provider(fake.clone())
        .build();
        let outbox = Arc::new(Outbox::new(link));
        let replies = Arc::new(Replies::new().outbound(outbox));
        let flow = flow_setup
            .connections(conns.clone())
            .build(env.modules.clone(), Arc::new(LinearExecutor));
        let ch = Channels::builder(env.modules.clone(), conns, Arc::new(flow.clone()))
            .adapter(fake.clone())
            .responder(replies)
            .flow(flow)
            .build();
        let (me, _) =
            FakeChannel::seed_connection(env.store(), &caller(WS, ME), Some("upx")).unwrap();
        fake.space(
            "G_NOISY",
            "告警交流群",
            SpaceKind::Group,
            false,
            &[("ulilei", "李雷")],
        );
        fake.space(
            "G_OTHER",
            "其他群",
            SpaceKind::Group,
            false,
            &[("ulilei", "李雷")],
        );
        fake.space("D_LI", "", SpaceKind::Dm, false, &[("ulilei", "李雷")]);
        env.store()
            .write(|w| {
                spaces::set_account_rules(
                    w,
                    &Viewer::bare(WS, ME),
                    &me,
                    &json!([{"when": {"kind": "mention_or_dm"}, "act": "draft"}]),
                    start,
                )
            })
            .unwrap();
        env.gateway.script(
            crate::compose::H_SAFETY,
            r#"{"text": "收到。", "needs_approval": false, "why": "回复"}"#,
        );
        Harness {
            env,
            _keys: keys,
            ch,
            me,
        }
    }

    async fn draft_and_ignore(h: &Harness, space_ext: &str, ext_id: &str, mention: bool) -> String {
        let now = h.env.clock.now();
        let mut msg: InboundMessage = FakeChannel::message(space_ext, ext_id, "ulilei", "看下这个");
        msg.created = now;
        if mention {
            msg.mentions.push(Mention {
                handle: None,
                name: "你".into(),
                is_connection: true,
            });
        }
        let inbound = h.ch.on_message(WS, &h.me, msg).await.unwrap();
        let dec = inbound.response.unwrap().decision_id.unwrap();
        decisions::dispatch(&h.env.modules, &caller(WS, ME), &dec, "no", "")
            .unwrap()
            .unwrap();
        inbound.space_id
    }

    fn open_space_suggestion(h: &Harness, space_id: &str) -> Option<steve_store::Decision> {
        h.env
            .store()
            .read(|r| DecisionRows(r).open_for(WS, SUBJECT_SPACE, space_id))
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn three_ignores_within_seven_days_triggers_record_only_suggestion_and_applies() {
        let h = harness();

        // 1st and 2nd ignores: below threshold (3), no suggestion.
        let space = draft_and_ignore(&h, "G_NOISY", "m1", true).await;
        assert!(open_space_suggestion(&h, &space).is_none());

        h.env.clock.advance(2 * 86_400_000);
        draft_and_ignore(&h, "G_NOISY", "m2", true).await;
        assert!(open_space_suggestion(&h, &space).is_none());

        // An ignore in a different space does NOT count toward G_NOISY.
        draft_and_ignore(&h, "G_OTHER", "m_other", true).await;
        assert!(open_space_suggestion(&h, &space).is_none());

        // 3rd ignore within 7 days triggers the "以后只记录" proposal.
        h.env.clock.advance(2 * 86_400_000);
        draft_and_ignore(&h, "G_NOISY", "m3", true).await;
        let sug = open_space_suggestion(&h, &space).expect("record_only suggestion opened");
        assert_eq!(sug.kind, "propose");
        assert_eq!(sug.options, vec!["record_only", "keep"]);
        assert_eq!(sug.result["kind"], SUGGESTION_KIND_IGNORE_RECORD_ONLY);
        assert_eq!(sug.result["category"], "mention");
        assert_eq!(sug.result["ignored_count"], IGNORE_SUGGEST_THRESHOLD);

        // Accepting "record_only" switches the space to record-only (muted + reply_as_you=record).
        decisions::dispatch(&h.env.modules, &caller(WS, ME), &sug.id, "record_only", "")
            .unwrap()
            .unwrap();
        let set = h.env.store().read(|r| spaces::settings(r, &space)).unwrap();
        assert!(set.muted);
        assert_eq!(set.reply_as_you, ReplyAsYou::Record);
        assert!(set.configured);

        // Undo restores previous flags cleanly.
        decisions::undo(&h.env.modules, &sug.id, ME, |_| true)
            .unwrap()
            .unwrap();
        let restored = h.env.store().read(|r| spaces::settings(r, &space)).unwrap();
        assert!(!restored.muted);
        assert_eq!(restored.reply_as_you, ReplyAsYou::Draft);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ignores_older_than_seven_days_do_not_trigger_and_keep_resets_window() {
        let h = harness();

        // Ignore #1 at day 0, #2 at day 4, #3 at day 8 (>7d after #1 -> only 2 inside 7d window).
        let space = draft_and_ignore(&h, "D_LI", "dm1", false).await;
        h.env.clock.advance(4 * 86_400_000);
        draft_and_ignore(&h, "D_LI", "dm2", false).await;
        h.env.clock.advance(4 * 86_400_000);
        draft_and_ignore(&h, "D_LI", "dm3", false).await;
        assert!(
            open_space_suggestion(&h, &space).is_none(),
            "only 2 ignores fall inside the 7-day rolling window"
        );

        // Ignore #4 at day 9 -> dm2 (day 4), dm3 (day 8), dm4 (day 9) = 3 within 7d -> suggests!
        h.env.clock.advance(86_400_000);
        draft_and_ignore(&h, "D_LI", "dm4", false).await;
        let sug = open_space_suggestion(&h, &space).expect("suggestion after 3rd ignore in window");

        // User picks "keep": resets baseline so a single extra ignore does NOT re-nag immediately.
        decisions::dispatch(&h.env.modules, &caller(WS, ME), &sug.id, "keep", "")
            .unwrap()
            .unwrap();
        h.env.clock.advance(60 * 60_000);
        draft_and_ignore(&h, "D_LI", "dm5", false).await;
        assert!(
            open_space_suggestion(&h, &space).is_none(),
            "keep resets the window until 3 new ignores occur"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dismiss_drafts_ignores_spaces_without_settings_or_connection() {
        let h = harness();
        steve_store::testkit::hub::views::seed_channel_tiers(h.env.store()).unwrap();
        let (star, quiet, failed) =
            steve_store::testkit::hub::views::channel_decisions(h.env.store()).unwrap();
        steve_store::testkit::hub::views::star_and_reply(h.env.store()).unwrap();
        steve_store::testkit::hub::views::remove_reply(h.env.store()).unwrap();

        // `seed_channel_tiers` inserts minimal spaces without `space_settings`: bulk-dismissing
        // quiet drafts must succeed instead of failing with NotFound in `on_message_ignored`.
        let dismissed = decisions::dismiss_drafts(
            &h.env.modules,
            &caller(WS, ME),
            &[star, quiet.clone(), failed],
        )
        .unwrap();
        assert_eq!(dismissed, vec![quiet]);
        assert!(open_space_suggestion(&h, "tier-space-b").is_none());
    }
}
