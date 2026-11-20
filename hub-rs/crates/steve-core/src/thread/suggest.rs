//! Suggestion cards: when the router is unsure whether a message is work, Steve asks
//! with a card — “在这里出方案 / 建成事项：<标题>” — instead of quietly making an item. Only in the
//! conversation (not in the inbox), never taken by YOLO, greyed out after 24 hours but still clickable.

use super::offer::{Handled, ThreadItem, not_found};
use super::research::{Inline, InlineJob};
use super::{decide::Decision, lock, meta_of};
use crate::Core;
use crate::error::{CoreError, Result};
use crate::gateway::Route;
use crate::repo::{FileRef, Item, ItemExt};
use crate::util::now;
use serde_json::{Value, json};

/// After this long an unanswered card is `expired`.
const EXPIRES: i64 = 24 * 3600 * 1000;

/// The request stays on the same card, with choices that do not reopen the finished item.
fn closed_card(name: &str, work: Value) -> (String, Value) {
    let q = format!("{name} 已经结束了。这条请求要单独建一个事项，还是在这里讨论？");
    (
        q.clone(),
        json!({
            "question": q,
            "choices": [work, {"id": "here", "label": "在这里讨论"}],
            "state": "open",
        }),
    )
}

/// The card's text and `meta.suggest` for a route the guards turned into a suggestion. About an existing
/// item (`item`, the one the model named): why the message belongs there, the recommendation, and the
/// decision that is actually open — append it to that item, or make an item of its own (decided by the
/// user on 2026-11-05, ST-44; no longer “这是给 ST-n 的吗？ 送到 ST-n / 在这里回答”).
pub(crate) fn card(r: &Route, d: &Decision, item: Option<&Item>) -> (String, Value) {
    if let Some(id) = &d.for_item {
        let name = item
            .map(|i| format!("{id}「{}」", i.title))
            .unwrap_or_else(|| id.clone());
        let why = r
            .why
            .trim()
            .trim_end_matches(['。', '.', '；', ';', '，', ',']);
        let because = if why.is_empty() {
            format!("这条和 {name} 是同一件事的补充")
        } else {
            why.to_string()
        };
        let q = format!(
            "{because}，可以和它一起实现和验收。建议追加到 {name}，你想追加进去，还是单独建一个事项？"
        );
        /* an item of its own goes where the item it was close to went, unless the router said otherwise */
        let title = Some(r.title.trim()).filter(|t| !t.is_empty());
        let goal = Some(r.goal.trim()).filter(|g| !g.is_empty());
        let project = r
            .project
            .clone()
            .or_else(|| item.and_then(|i| i.project_id.clone()));
        let bot = Some(r.bot.clone())
            .filter(|b| b != "steve" || r.bot_from_project)
            .or_else(|| {
                item.filter(|i| i.project_id == project)
                    .and_then(|i| i.bot_id.clone())
            });
        if item.is_some_and(|i| i.is_finished()) {
            return closed_card(
                &name,
                json!({"id": "new", "label": "单独建项", "title": title, "goal": goal,
                "project": project, "bot": bot, "tags": r.tag_ids}),
            );
        }
        return (
            q.clone(),
            json!({
                "question": q,
                "choices": [
                    {"id": "item", "label": format!("追加到 {id}"), "item": id, "recommended": true},
                    {"id": "new", "label": "单独建项", "title": title, "goal": goal,
                     "project": project, "bot": bot, "tags": r.tag_ids},
                ],
                "state": "open",
            }),
        );
    }
    let q = if r.reply.trim().is_empty() || r.fallback {
        "这件事要我先在这里出个方案，还是直接建成事项交给 Bot 去做？".to_string()
    } else {
        r.reply.trim().to_string()
    };
    (
        q.clone(),
        json!({
            "question": q,
            "choices": [
                {"id": "here", "label": "在这里出方案"},
                {"id": "item", "label": format!("建成事项：{}", r.title), "title": r.title, "goal": r.goal,
                 "project": r.project, "bot": r.bot, "tags": r.tag_ids},
            ],
            "state": "open",
        }),
    )
}

impl Core {
    /// Cards nobody answered for 24 hours turn `expired` (`route_log.outcome = suggest_expired`).
    pub(crate) fn expire_suggestions(&self, ws: &str, space: &str) -> Result<()> {
        let th = self.store().store().thread();
        for m in self.conv_messages(ws, space)? {
            if m.meta["suggest"]["state"] == "open" && now() - m.created >= EXPIRES {
                let mut meta = meta_of(&m);
                meta["suggest"]["state"] = json!("expired");
                self.store()
                    .message_patch_meta(&m.id, &Value::Object(meta))?;
                if let Some(o) = m.meta.get("origin").and_then(Value::as_str) {
                    th.route_outcome(o, "suggest_expired", None)?;
                }
            }
        }
        Ok(())
    }

    /// The user picked on a suggestion card (`POST /api/thread/:id/suggest`): `here` answers the original
    /// message in place (the answer continues this card), `item` makes the item, or appends the message to
    /// the item a card about an existing item recommends; `new` (that card's other choice) makes an item
    /// of its own.
    /// `by`: the member who picked, whose access an item made here runs with.
    pub fn suggest(
        &self,
        ws: &str,
        msg_id: &str,
        choice: &str,
        by: Option<&str>,
    ) -> Result<Handled> {
        if !matches!(choice, "here" | "item" | "new") {
            return Err(CoreError::Status(
                400,
                steve_i18n::t!("core.error.invalid", fields = "choice"),
            ));
        }
        let _one = lock(&self.0.thread.acting);
        let m = self
            .store()
            .message(msg_id)?
            .filter(|m| m.workspace_id == ws)
            .ok_or_else(not_found)?;
        let Some(card) = m.meta.get("suggest").filter(|s| s.is_object()).cloned() else {
            return Err(CoreError::Status(
                400,
                steve_i18n::t!("core.message.not_suggestion"),
            ));
        };
        if !matches!(card["state"].as_str(), Some("open" | "expired")) {
            let item = card["item"]
                .as_str()
                .and_then(|i| self.store().item(i).ok().flatten());
            return Ok(Handled {
                message_id: card["answer"].as_str().unwrap_or(msg_id).into(),
                item,
                already: true,
            });
        }
        let origin = self.origin_of(ws, &m)?;
        let om = self.store().message(&origin)?.ok_or_else(not_found)?;
        let files: Vec<FileRef> = om
            .meta
            .get("files")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let th = self.store().store().thread();
        let pick = card["choices"]
            .as_array()
            .and_then(|cs| cs.iter().find(|c| c["id"] == choice))
            .cloned()
            .ok_or_else(|| CoreError::Status(400, steve_i18n::t!("core.message.no_choice")))?;
        let mut meta = meta_of(&m);
        meta["suggest"]["state"] = json!("chosen");
        meta["suggest"]["chosen"] = json!(choice);
        if choice == "here" {
            let job = InlineJob {
                kind: Inline::Research,
                origin: origin.clone(),
                question: om.text.clone(),
                files,
                earlier: vec![],
                user: None,
                project: m.meta["steps"]["route"]["project"]
                    .as_str()
                    .map(String::from),
                route: m.meta["steps"].get("route").cloned(),
                offer: None,
                reply_to: Some(msg_id.into()),
                run: None,
                stop: super::Stop::default(),
                t0: now(),
                resume: None,
                space: m.space.clone(),
                to_item: None,
            };
            if !self.enqueue_inline(ws, job)? {
                return Err(CoreError::Status(
                    409,
                    steve_i18n::t!("core.thread.queue_full"),
                ));
            }
            self.store()
                .message_patch_meta(msg_id, &Value::Object(meta))?;
            th.route_outcome(&origin, "suggest_here", None)?;
            return Ok(Handled {
                message_id: msg_id.into(),
                item: None,
                already: false,
            });
        }
        /* a card asking whether the message was for an existing item */
        if let Some(target) = pick["item"]
            .as_str()
            .filter(|_| pick.get("title").is_none())
        {
            if !self.forward_open(target, &om.text, &files, &origin)? {
                let work = card["choices"]
                    .as_array()
                    .and_then(|cs| cs.iter().find(|c| c["id"] == "new"))
                    .cloned()
                    .unwrap_or_else(|| json!({"id": "new", "label": "单独建项", "goal": om.text}));
                let (q, next) = closed_card(target, work);
                meta["suggest"] = next;
                self.store().store().messages().patch_text_meta(
                    msg_id,
                    &q,
                    &Value::Object(meta),
                )?;
                th.route_outcome(&origin, "suggest_closed", None)?;
                return Ok(Handled {
                    message_id: msg_id.into(),
                    item: None,
                    already: false,
                });
            }
            meta["suggest"]["item"] = json!(target);
            self.store()
                .message_patch_meta(msg_id, &Value::Object(meta))?;
            th.route_outcome(&origin, "suggest_item", Some(target))?;
            let id = self.store().message_add_in(
                ws,
                &m.space,
                "steve",
                &format!("已作为补充送到 {target}。"),
                Some(target),
                &json!({"origin": origin, "reply_to": msg_id}),
            )?;
            super::flowback::adopt_quiet(self.store(), target, &id);
            return Ok(Handled {
                message_id: id,
                item: self.store().item(target)?,
                already: false,
            });
        }
        let s = |k: &str| pick[k].as_str().map(String::from);
        let item = self.thread_item(ThreadItem {
            ws,
            origin: &origin,
            title: s("title").unwrap_or_else(|| crate::util::js_head(&om.text, 24).into()),
            goal: s("goal").unwrap_or_else(|| om.text.clone()),
            bot: s("bot"),
            project: s("project"),
            tags: pick["tags"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            files,
            from_question: super::decide::pure_question(&om.text),
            by: by.map(String::from),
            answer: Some(&m),
            start: true, // your action in the web
        })?;
        meta["suggest"]["item"] = json!(item.id);
        self.store()
            .message_patch_meta(msg_id, &Value::Object(meta))?;
        th.route_outcome(&origin, "suggest_item", Some(&item.id))?;
        let text = format!("好，建了 {}，交给 {} 了。", item.id, self.bot_name(&item));
        let id = self.announce(
            ws,
            &item,
            &origin,
            &text,
            serde_json::Map::from_iter([("reply_to".to_string(), json!(msg_id))]),
        )?;
        Ok(Handled {
            message_id: id,
            item: Some(item),
            already: false,
        })
    }
}

#[cfg(test)]
mod sg24_tests {
    use super::*;
    use crate::repo::WS;
    use crate::testkit::{FakeResearch, Kit, Step};
    use steve_runtime::testkit::eventually_sync;

    fn fixture(k: &Kit, state: &str, choices: Value, files: Value) -> (String, String) {
        let origin = k
            .db()
            .message_add(
                WS,
                "me",
                "Original request, keep its text and attachments",
                None,
                Some(&json!({"files":files})),
            )
            .unwrap();
        let card = k.db().message_add(WS, "steve", "suggestion", None,
            Some(&json!({"origin":origin,"keep":"card","suggest":{"state":state,"choices":choices}}))).unwrap();
        k.db()
            .store()
            .thread()
            .route_add(&steve_store::NewRouteLog {
                workspace_id: WS.into(),
                message_id: origin.clone(),
                kind: "suggest".into(),
                model_kind: "item".into(),
                ..Default::default()
            })
            .unwrap();
        (origin, card)
    }

    fn status(error: CoreError, expected: u16) {
        match error {
            CoreError::Status(code, message) => {
                assert_eq!(code, expected);
                assert!(!message.is_empty());
            }
            other => panic!("unexpected error {other}"),
        }
    }

    #[tokio::test]
    async fn sg24_suggest_validates_before_writes_and_finished_cards_are_idempotent() {
        let k = Kit::new();
        status(
            k.core.suggest(WS, "missing", "invalid", None).unwrap_err(),
            400,
        );
        status(
            k.core.suggest(WS, "missing", "here", None).unwrap_err(),
            404,
        );
        let plain = k.db().message_add(WS, "me", "plain", None, None).unwrap();
        status(k.core.suggest(WS, &plain, "item", None).unwrap_err(), 400);
        let (origin, card) = fixture(&k, "open", json!([{"id":"item"}]), Value::Null);
        let before = k.db().message(&card).unwrap().unwrap().meta;
        status(
            k.core.suggest("foreign", &card, "item", None).unwrap_err(),
            404,
        );
        status(k.core.suggest(WS, &card, "here", None).unwrap_err(), 400);
        assert_eq!(k.db().message(&card).unwrap().unwrap().meta, before);
        assert!(
            k.db()
                .store()
                .thread()
                .route_of(&origin)
                .unwrap()
                .unwrap()
                .outcome
                .is_none()
        );
        let item = k.item("run", "执行", None);
        for (state, answer, target) in [
            (json!("chosen"), Some("answer-id"), Some(item.as_str())),
            (json!("dismissed"), None, Some("missing")),
            (Value::Null, None, None),
        ] {
            let mut meta = before.clone();
            meta["origin"] = json!("unreadable-origin");
            meta["suggest"]["state"] = state;
            if let Some(answer) = answer {
                meta["suggest"]["answer"] = json!(answer);
            }
            if let Some(target) = target {
                meta["suggest"]["item"] = json!(target);
            }
            k.db().message_patch_meta(&card, &meta).unwrap();
            let h = k.core.suggest(WS, &card, "new", None).unwrap();
            assert!(h.already);
            assert_eq!(h.message_id, answer.unwrap_or(&card));
            assert_eq!(
                h.item.map(|i| i.id),
                target.filter(|t| *t == item).map(String::from)
            );
            assert_eq!(k.db().message(&card).unwrap().unwrap().meta, meta);
        }
        let mut meta = before;
        meta["origin"] = json!("missing-origin");
        k.db().message_patch_meta(&card, &meta).unwrap();
        status(k.core.suggest(WS, &card, "here", None).unwrap_err(), 404);
        assert_eq!(k.db().items(WS).unwrap().len(), 1);
        assert!(k.rt.st().prompts.is_empty());
    }

    #[tokio::test]
    async fn sg24_suggest_forwards_existing_work_once_and_adopts_the_acknowledgement() {
        let k = Kit::new();
        let item = k.item("run", "执行", None);
        let session = k.session(&item, "idle");
        let file = k.file("context.txt", "text/plain", b"context");
        let (origin, card) = fixture(
            &k,
            "expired",
            json!([{"id":"item","item":item}]),
            json!([file]),
        );
        k.db()
            .store()
            .thread()
            .item_add(&item, WS, &origin, Some(&card), false)
            .unwrap();
        let h = k.core.suggest(WS, &card, "item", None).unwrap();
        assert!(!h.already);
        assert_eq!(h.item.unwrap().id, item);
        let ack = k.db().message(&h.message_id).unwrap().unwrap();
        assert_eq!(ack.text, format!("已作为补充送到 {item}。"));
        assert_eq!(ack.meta["origin"], origin);
        assert_eq!(ack.meta["reply_to"], card);
        assert_eq!(ack.meta["card"]["item"], item);
        let meta = k.db().message(&card).unwrap().unwrap().meta;
        assert_eq!(meta["suggest"]["state"], "chosen");
        assert_eq!(meta["suggest"]["chosen"], "item");
        assert_eq!(meta["suggest"]["item"], item);
        assert_eq!(meta["keep"], "card");
        assert_eq!(
            k.db()
                .store()
                .thread()
                .route_of(&origin)
                .unwrap()
                .unwrap()
                .outcome
                .as_deref(),
            Some("suggest_item")
        );
        assert_eq!(
            k.rt.prompts_of(&session),
            ["补充信息：Original request, keep its text and attachments"]
        );
        assert!(k.rt.st().files.iter().any(|(_, fs)| fs.contains(&file)));
        let count = k.db().messages(WS).unwrap().len();
        let again = k.core.suggest(WS, &card, "new", None).unwrap();
        assert!(again.already);
        assert_eq!(again.message_id, card);
        assert_eq!(k.db().messages(WS).unwrap().len(), count);
    }

    #[tokio::test]
    async fn sg24_suggest_closed_target_reopens_the_same_card_without_losing_the_new_choice() {
        for retained in [false, true] {
            let k = Kit::new();
            let item = k.item("done", "收尾", None);
            let new = json!({"id":"new","label":"Separate","title":"Kept title","goal":"Kept goal","bot":"dev","tags":["kept"]});
            let mut choices = vec![json!({"id":"item","item":item})];
            if retained {
                choices.push(new.clone());
            }
            let (origin, card) = fixture(&k, "open", json!(choices), json!({"invalid":"files"}));
            let h = k.core.suggest(WS, &card, "item", None).unwrap();
            assert_eq!(h.message_id, card);
            assert!(!h.already);
            assert!(h.item.is_none());
            let m = k.db().message(&card).unwrap().unwrap();
            assert_eq!(
                m.text,
                format!("{item} 已经结束了。这条请求要单独建一个事项，还是在这里讨论？")
            );
            assert_eq!(m.meta["keep"], "card");
            assert_eq!(m.meta["suggest"]["state"], "open");
            assert_eq!(
                m.meta["suggest"]["choices"][0],
                if retained {
                    new
                } else {
                    json!({"id":"new","label":"单独建项","goal":"Original request, keep its text and attachments"})
                }
            );
            assert_eq!(
                m.meta["suggest"]["choices"][1],
                json!({"id":"here","label":"在这里讨论"})
            );
            assert_eq!(
                k.db()
                    .store()
                    .thread()
                    .route_of(&origin)
                    .unwrap()
                    .unwrap()
                    .outcome
                    .as_deref(),
                Some("suggest_closed")
            );
            assert_eq!(k.db().items(WS).unwrap().len(), 1);
            assert!(k.rt.st().prompts.is_empty());
        }
    }

    #[tokio::test]
    async fn sg24_suggest_new_work_preserves_choice_fields_fallbacks_and_requester() {
        for explicit in [false, true] {
            let k = Kit::new();
            let file = k.file("request.txt", "text/plain", b"request");
            let user = k
                .db()
                .store()
                .users()
                .create("sg24-owner", "Owner", None, "owner")
                .unwrap();
            let pick = if explicit {
                json!({
                    "id":"new","item":"not-a-forward","title":"Chosen title","goal":"Chosen goal",
                    "bot":"dev","tags":k.task_tags(WS)
                })
            } else {
                json!({"id":"item"})
            };
            let (origin, card) = fixture(
                &k,
                if explicit { "expired" } else { "open" },
                json!([pick]),
                json!([file]),
            );
            let choice = if explicit { "new" } else { "item" };
            let h = k.core.suggest(WS, &card, choice, Some(&user)).unwrap();
            let item = h.item.unwrap();
            assert!(!h.already);
            assert_eq!(
                item.title,
                if explicit {
                    "Chosen title"
                } else {
                    "Original request, keep i"
                }
            );
            assert_eq!(
                k.db()
                    .goals(&item.id)
                    .unwrap()
                    .last()
                    .unwrap()
                    .text
                    .lines()
                    .next(),
                Some(if explicit {
                    "Chosen goal"
                } else {
                    "Original request, keep its text and attachments"
                })
            );
            assert_eq!(item.source, ("对话".into(), "chat".into()));
            if explicit {
                assert_eq!(item.bot_id.as_deref(), Some("dev"));
            }
            let link = k.db().store().thread().item(&item.id).unwrap().unwrap();
            assert_eq!(link.origin_msg, origin);
            assert_eq!(link.reply_msg.as_deref(), Some(h.message_id.as_str()));
            let ack = k.db().message(&h.message_id).unwrap().unwrap();
            assert_eq!(ack.meta["reply_to"], card);
            assert_eq!(ack.meta["card"]["item"], item.id);
            assert_eq!(
                k.db().message(&card).unwrap().unwrap().meta["suggest"]["item"],
                item.id
            );
            assert_eq!(
                k.db()
                    .store()
                    .thread()
                    .route_of(&origin)
                    .unwrap()
                    .unwrap()
                    .outcome
                    .as_deref(),
                Some("suggest_item")
            );
            let actor: Option<String> = k
                .db()
                .store()
                .reader(|c| -> steve_store::Result<_> {
                    Ok(c.query_row(
                        "SELECT requested_by FROM items WHERE id=?",
                        [&item.id],
                        |r| r.get(0),
                    )?)
                })
                .unwrap();
            assert_eq!(actor.as_deref(), Some(user.as_str()));
            assert!(k.rt.st().files.iter().any(|(_, fs)| fs.contains(&file)));
            let count = k.db().items(WS).unwrap().len();
            assert!(
                k.core
                    .suggest(WS, &card, choice, Some(&user))
                    .unwrap()
                    .already
            );
            assert_eq!(k.db().items(WS).unwrap().len(), count);
        }
    }

    #[tokio::test]
    async fn sg24_suggest_here_uses_the_durable_queue_and_refusal_leaves_the_card_open() {
        let k = Kit::new();
        let research = FakeResearch::new(vec![Step::Gate, Step::Text("Answer")]);
        assert!(k.core.set_researcher(research.clone()));
        let file = k.file("context.txt", "text/plain", b"context");
        let mut cards = vec![];
        for _ in 0..=super::super::research::MAX_WAITING + 1 {
            cards.push(fixture(&k, "open", json!([{"id":"here"}]), json!([file])));
        }
        let first = k.core.suggest(WS, &cards[0].1, "here", None).unwrap();
        assert_eq!(first.message_id, cards[0].1);
        assert!(first.item.is_none());
        assert!(
            eventually_sync("first inline research started", || research
                .seen
                .lock()
                .unwrap()
                .len()
                == 1)
            .await
        );
        for (_, card) in cards
            .iter()
            .skip(1)
            .take(super::super::research::MAX_WAITING)
        {
            k.core.suggest(WS, card, "here", None).unwrap();
        }
        let (origin, refused) = cards.last().unwrap();
        let before = k.db().message(refused).unwrap().unwrap().meta;
        status(k.core.suggest(WS, refused, "here", None).unwrap_err(), 409);
        assert_eq!(k.db().message(refused).unwrap().unwrap().meta, before);
        assert!(
            k.db()
                .store()
                .thread()
                .route_of(origin)
                .unwrap()
                .unwrap()
                .outcome
                .is_none()
        );
        assert!(
            k.db()
                .store()
                .thread()
                .runs_of_message(origin)
                .unwrap()
                .is_empty()
        );
        assert!(k.db().items(WS).unwrap().is_empty());
        research.open(cards.len());
        assert!(
            eventually_sync("accepted research drained", || k
                .db()
                .store()
                .thread()
                .runs_active(WS)
                .unwrap()
                .is_empty())
            .await
        );
        assert_eq!(research.seen.lock().unwrap().len(), cards.len() - 1);
        assert_eq!(research.seen.lock().unwrap()[0].files, [file]);
        for (origin, card) in cards.iter().take(cards.len() - 1) {
            assert_eq!(
                k.db()
                    .store()
                    .thread()
                    .route_of(origin)
                    .unwrap()
                    .unwrap()
                    .outcome
                    .as_deref(),
                Some("suggest_here")
            );
            assert_eq!(
                k.db()
                    .store()
                    .thread()
                    .runs_of_message(origin)
                    .unwrap()
                    .len(),
                1
            );
            assert!(k.core.suggest(WS, card, "here", None).unwrap().already);
        }
    }

    #[tokio::test]
    async fn sg24_suggest_patch_failure_keeps_the_already_created_item_and_original_card() {
        let k = Kit::new();
        let (origin, card) = fixture(
            &k,
            "open",
            json!([{"id":"item","title":"Created first","bot":"dev"}]),
            Value::Null,
        );
        let before = k.db().message(&card).unwrap().unwrap().meta;
        k.db().exec("CREATE TRIGGER sg24_refuse_suggest BEFORE UPDATE OF meta ON messages WHEN OLD.text='suggestion' BEGIN SELECT RAISE(ABORT,'sg24 suggest refused'); END", &[]).unwrap();
        assert!(
            k.core
                .suggest(WS, &card, "item", None)
                .unwrap_err()
                .to_string()
                .contains("sg24 suggest refused")
        );
        assert_eq!(k.db().items(WS).unwrap().len(), 1);
        assert_eq!(k.db().items(WS).unwrap()[0].title, "Created first");
        assert_eq!(k.db().message(&card).unwrap().unwrap().meta, before);
        assert!(
            k.db()
                .store()
                .thread()
                .route_of(&origin)
                .unwrap()
                .unwrap()
                .outcome
                .is_none()
        );
    }
}
