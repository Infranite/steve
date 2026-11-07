//! One message on the workspace lane: route it, let the guards decide, log the routing, and act —
//! queue an answer or research, make an item, show a card, send it to an item, or correct the last routing.

use super::decide::{self, DecideCtx};
use super::offer::{ThreadItem, confirms};
use super::research::{Inline, InlineJob, QUEUE_FULL};
use super::suggest;
use super::{AT, Job, REF, Steps, lock, names_bot, route_step};
use crate::Core;
use crate::error::Result;
use crate::gateway::{Route, Said};
use crate::repo::{FileRef, HubEvent, ItemExt};
use crate::util::{js_head, now};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::sync::{LazyLock, Mutex};
use steve_modules::contacts::item_people::Mention;
use steve_store::NewRouteLog;

/// Posts Steve's reply unless the user stopped it; the stored message's id.
type Reply<'a> = &'a (dyn Fn(&str, Option<&str>, Option<Value>) -> Option<String> + Sync);

/// “别建事项，直接在这告诉我”: a spoken correction back into the conversation.
pub(super) static TO_INLINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"别建|不用建|不要建|不建事项|别做成|不用做成|直接(在这里?|在对话里)?(告诉|说|讲|回答)|在这里?(告诉|说|讲|回答)")
        .expect("valid regex")
});

/// Asking how an item is going, not telling it something.
pub(super) static PROGRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"进展|进度|做完没|做完了吗|完成了吗|怎么样了|到哪了|到哪一步").expect("valid regex")
});

/// How long a spoken “别建事项” reaches back.
const REROUTE_WINDOW: i64 = 10 * 60 * 1000;

fn status_label(s: &str) -> &str {
    match s {
        "run" => "进行中",
        "wait" => "等你处理",
        "block" => "等前置事项",
        "pause" => "暂停了",
        "done" => "已完成",
        "drop" => "已放弃",
        "propose" => "等你确认",
        other => other,
    }
}

/// The items the routing made or touched, for `route_log`.
struct Logged {
    item: Option<String>,
}

impl Core {
    pub(super) async fn handle(&self, ws: &str, j: &Job) {
        let (mid, text, files, user) = (
            j.mid.as_str(),
            j.text.as_str(),
            &j.files[..],
            j.user.as_deref(),
        );
        let space = j.conv.space.as_str();
        let lane = if j.direct {
            super::direct_lane(ws, space)
        } else {
            super::lane_key(ws, space)
        };
        let flag = super::Stop::default();
        lock(&self.0.thread.inflight).insert(lane.clone(), flag.clone());
        /* the web's “thinking” line is about your conversation only, and stays with the turn in progress
        while a message with a fixed way passes it */
        let quiet = j.conv.guest() || (j.direct && self.main_busy_besides(ws));
        if !quiet {
            self.store().publish(HubEvent::SteveThinking {
                on: true,
                ws: Some(ws.into()),
                phase: None,
            });
        }
        /* what Steve did for this message, kept on the reply as meta.steps so the conversation can show it folded */
        let steps = Mutex::new(Steps::default());
        let t0 = now();
        let reply = |t: &str, item: Option<&str>, meta: Option<Value>| {
            if flag.is_stopped() {
                return None;
            }
            let mut m = match meta {
                Some(Value::Object(m)) => m,
                _ => Map::new(),
            };
            m.entry("origin").or_insert_with(|| json!(mid));
            m.insert("steps".into(), lock(&steps).json(now() - t0));
            match self
                .store()
                .message_add_in(ws, space, "steve", t, item, &Value::Object(m))
            {
                Ok(id) => {
                    let th = self.store().store().thread();
                    if let Err(e) = th.route_reply(mid, &id, item) {
                        tracing::error!("route log of {mid}: {e}");
                    }
                    /* an item's announcement is told once its card is in place (on_item) */
                    if item.is_none() {
                        self.said(ws, space, &id);
                    }
                    Some(id)
                }
                Err(e) => {
                    tracing::error!("reply {e}");
                    None
                }
            }
        };
        if let Err(e) = self
            .handle_inner(
                ws,
                mid,
                text,
                files,
                user,
                &j.mentions,
                &j.to,
                &j.conv,
                &flag,
                &reply,
                &steps,
                quiet,
            )
            .await
        {
            reply(
                &format!("（出错了：{}）", js_head(&e.to_string(), 120)),
                None,
                Some(json!({"error": true})),
            );
        }
        lock(&self.0.thread.inflight).remove(&lane);
        if !quiet {
            self.store().publish(HubEvent::SteveThinking {
                on: false,
                ws: Some(ws.into()),
                phase: None,
            });
        }
    }

    /// “已作为补充送到 ST-n。” under the user's message to an item: the item's card moves there, so it
    /// sits after the user's latest message about it and the reply grows on it.
    fn acknowledge(&self, id: &str, reply: Reply<'_>) {
        if let Some(m) = reply(&format!("已作为补充送到 {id}。"), Some(id), None) {
            super::flowback::adopt_quiet(self.store(), id, &m);
        }
    }

    /// One `route_log` row for a message that was not routed by the model (`/汇报`, `#ST-n`).
    fn log_fixed(&self, ws: &str, mid: &str, kind: &str, item: Option<&str>) -> Result<()> {
        self.store().store().thread().route_add(&NewRouteLog {
            workspace_id: ws.into(),
            message_id: mid.into(),
            model_kind: kind.into(),
            kind: kind.into(),
            confidence: Some(1.0),
            item_id: item.map(String::from),
            ..Default::default()
        })?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_inner(
        &self,
        ws: &str,
        mid: &str,
        text: &str,
        files: &[FileRef],
        user: Option<&str>,
        mentions: &[Mention],
        j_to: &super::SayTo,
        conv: &super::Conv,
        flag: &super::Stop,
        reply: Reply<'_>,
        steps: &Mutex<Steps>,
        quiet: bool,
    ) -> Result<()> {
        let db = self.store();
        let guest = conv.guest();
        if !guest && text.trim() == "/汇报" {
            self.log_fixed(ws, mid, "status", None)?;
            let i = Steps::begin(steps, "digest", json!({}));
            let mut d = Steps::end(steps, i, self.digest_text(ws), |_| json!({}))?;
            /* what the modules noted for the report (steve-flow's `digest` steps) */
            let notes = self
                .modules()
                .map(|m| m.registry.take_digest(ws))
                .unwrap_or_default();
            for n in notes {
                d.push_str(&format!("\n· {n}"));
            }
            reply(&d, None, None);
            return Ok(());
        }
        /* sent to an item (focus bar) or quoting one of its messages: straight there, no router */
        let target = match (&j_to.item, &j_to.reply_to) {
            (Some(i), _) => Some(i.clone()),
            (None, Some(r)) => db.message(r)?.and_then(|m| m.item_id),
            _ => None,
        };
        if let Some(id) = target.filter(|_| !guest)
            && db.item(&id)?.is_some_and(|i| i.workspace_id == ws)
        {
            if PROGRESS.is_match(text) {
                self.log_fixed(ws, mid, "status", Some(&id))?;
                reply(&self.status_text(&id)?, None, Some(json!({"status": id})));
                return Ok(());
            }
            self.log_fixed(ws, mid, "supplement", Some(&id))?;
            if !quiet {
                self.phase(ws, "supplement");
            }
            let i = Steps::begin(
                steps,
                "supplement",
                json!({"item": id, "files": files.len()}),
            );
            Steps::end(steps, i, self.forward(&id, text, files, mid), |_| json!({}))?;
            self.acknowledge(&id, reply);
            return Ok(());
        }
        /* “#ST-n …” always goes to the item; a bare “ST-n …” unless it asks about it (“ST-n 进展怎样了？”) */
        if let Some(c) = REF.captures(text).filter(|_| !guest)
            && (text.starts_with('#') || !decide::pure_question(text))
        {
            let id = c[1].to_string();
            if db.item(&id)?.is_some_and(|i| i.workspace_id == ws) {
                let rest = REF.replace(text, "");
                let rest = if rest.is_empty() {
                    text.to_string()
                } else {
                    rest.to_string()
                };
                if PROGRESS.is_match(&rest) {
                    self.log_fixed(ws, mid, "status", Some(&id))?;
                    reply(&self.status_text(&id)?, None, Some(json!({"status": id})));
                    return Ok(());
                }
                self.log_fixed(ws, mid, "supplement", Some(&id))?;
                if !quiet {
                    self.phase(ws, "supplement");
                }
                let i = Steps::begin(
                    steps,
                    "supplement",
                    json!({"item": id, "files": files.len()}),
                );
                Steps::end(
                    steps,
                    i,
                    self.forward(&id, &rest, files, mid),
                    |_| json!({}),
                )?;
                self.acknowledge(&id, reply);
                return Ok(());
            }
        }
        if let Err(e) = self.expire_suggestions(ws, &conv.space) {
            tracing::warn!("expire suggestion cards in {ws}: {e}");
        }
        let history = self.history_before(ws, mid)?;
        /* the router only sees that there are attachments; the bot that does the work gets the files themselves */
        let routed = if files.is_empty() {
            text.to_string()
        } else {
            let list = files
                .iter()
                .map(|f| format!("{} · {}", f.name, f.mime.split(';').next().unwrap_or("")))
                .collect::<Vec<_>>()
                .join("、");
            format!("{text}\n（附带 {} 个附件：{list}）", files.len())
        };
        /* a plain question needs no router: its kind is known (#52) */
        let plain = match decide::without_router(text, !files.is_empty(), guest, &[]) {
            Some(_) => {
                let c = self.route_ctx_in(ws, guest)?;
                decide::without_router(text, !files.is_empty(), guest, &c.focus).map(|kind| {
                    let project = c.steve_project.clone().or_else(|| {
                        c.projects
                            .iter()
                            .find(|p| !p.archived)
                            .map(|p| p.id.clone())
                    });
                    Route {
                        kind: kind.into(),
                        confidence: 1.0,
                        why: "纯问题，没有问路由".into(),
                        ..Route::fallback(text, project)
                    }
                    .plain()
                })
            }
            None => None,
        };
        if !guest && plain.is_none() {
            self.phase(ws, "route");
        }
        let r0 = now();
        /* stopping does not wait for the router: nothing has happened yet */
        let r = match plain.clone() {
            Some(r) => r,
            None => tokio::select! {
                r = self.route_in(ws, &routed, &history, false, guest) => r?,
                _ = flag.stopped() => return Ok(()),
            },
        };
        let ms = now() - r0;
        if flag.is_stopped() {
            return Ok(());
        }
        /* “@名字 …” picks the bot explicitly */
        let at = AT.captures(text).map(|c| c[1].to_string());
        let named = match at.as_ref().filter(|_| !guest || conv.items) {
            Some(a) => db
                .bots(ws)?
                .into_iter()
                .find(|b| b.enabled && names_bot(&b.name, a)),
            None => None,
        };
        let mut d = decide::decide(
            &r,
            text,
            DecideCtx {
                named_bot: named.is_some(),
            },
        );
        if guest {
            d = decide::for_guest(d, &r, conv.research, conv.items);
        } else {
            d = decide::for_main(d, &r);
        }
        d = decide::for_owner(d, conv.owner_only && conv.research);
        if plain.is_some() {
            d.guard = Some("q");
        }
        lock(steps).route = Some(route_step(&r, Some(&d), ms));
        self.store().store().thread().route_add(&NewRouteLog {
            workspace_id: ws.into(),
            message_id: mid.into(),
            model: r.model.clone(),
            model_kind: r.kind.clone(),
            kind: d.kind.clone(),
            confidence: (!r.fallback).then_some(r.confidence),
            signals: r.signals.clone(),
            guard: d.guard.map(String::from),
            fallback: r.fallback,
            ms,
            ..Default::default()
        })?;
        let logged = match d.kind.as_str() {
            "answer" | "research" => {
                /* a follow-up of the research answer just given continues its session */
                let resume = self.resume_for(ws, mid)?;
                /* only a research run resumes the session that already saw them; an answer starts fresh */
                let resumed = resume.as_ref().filter(|_| d.kind == "research");
                let earlier = self.earlier_files(ws, mid, files, resumed)?;
                let job = InlineJob {
                    kind: if d.kind == "research" {
                        Inline::Research
                    } else {
                        Inline::Answer
                    },
                    origin: mid.into(),
                    question: text.into(),
                    files: files.to_vec(),
                    earlier,
                    user: user.map(String::from),
                    project: r.project.clone(),
                    route: lock(steps).route.clone(),
                    offer: d.offer.clone(),
                    reply_to: None,
                    run: None,
                    stop: super::Stop::default(),
                    t0: now(),
                    resume,
                    space: conv.space.clone(),
                };
                if !self.enqueue_inline(ws, job)? {
                    reply(QUEUE_FULL, None, None);
                }
                Logged { item: None }
            }
            "config" => {
                let request = with_earlier(text, &history);
                self.on_config(ws, &request, user, reply, steps).await?
            }
            "suggest" => {
                let item = match &d.for_item {
                    Some(i) => self.store().item(i)?,
                    None => None,
                };
                let (q, card) = suggest::card(&r, &d, item.as_ref());
                reply(&q, None, Some(json!({"suggest": card})));
                Logged { item: None }
            }
            "ask" => {
                let q = if r.reply.trim().is_empty() {
                    "你指的是哪个？".to_string()
                } else {
                    r.reply.trim().to_string()
                };
                reply(&q, None, Some(json!({"ask": {"options": r.options}})));
                Logged { item: None }
            }
            "supplement" => {
                let id = r.item.clone().unwrap_or_default();
                self.phase(ws, "supplement");
                let i = Steps::begin(
                    steps,
                    "supplement",
                    json!({"item": id, "files": files.len()}),
                );
                Steps::end(steps, i, self.forward(&id, text, files, mid), |_| json!({}))?;
                self.acknowledge(&id, reply);
                Logged { item: Some(id) }
            }
            "status" => {
                let id = r.item.clone().unwrap_or_default();
                reply(&self.status_text(&id)?, None, Some(json!({"status": id})));
                Logged { item: Some(id) }
            }
            "reroute" => self.spoken_reroute(ws, mid, text, &r, user, reply).await?,
            _ => self.on_item(
                ws, mid, text, files, user, mentions, &r, named, conv.start, reply, steps,
            )?,
        };
        if let Some(i) = logged.item {
            let th = self.store().store().thread();
            if let Some(l) = th.route_of(mid)?
                && l.item_id.is_none()
                && let Some(reply_id) = l.reply_id
            {
                th.route_reply(mid, &reply_id, Some(&i))?;
            }
        }
        Ok(())
    }

    /// A standing setting said in words (router kind `config`): every configuration it touches is
    /// drafted as a card to apply — Steve's own settings, bots, projects and tags, and with a known user
    /// the modules' (YOLO, schedules, flows, knowledge sources, mailboxes…) — or, when the user only asks,
    /// shown as it is now.
    async fn on_config(
        &self,
        ws: &str,
        text: &str,
        user: Option<&str>,
        reply: Reply<'_>,
        steps: &Mutex<Steps>,
    ) -> Result<Logged> {
        self.phase(ws, "config");
        let i = Steps::begin(
            steps,
            "draft_config",
            json!({"request": js_head(text, 200)}),
        );
        let res = self.config_request(ws, text, user).await;
        let (ask, ds) = Steps::end(steps, i, res, |(_, ds)| {
            json!({
                "targets": ds.iter().map(|d| d.title.clone()).collect::<Vec<_>>(),
                "changes": ds
                    .iter()
                    .filter_map(|d| d.proposal.as_ref())
                    .map(|p| p["changes"].as_array().map_or(0, Vec::len))
                    .sum::<usize>(),
            })
        })?;
        /* the L0 ones apply right away: their cards say 已应用 and offer the undo */
        let applied = if ask {
            vec![]
        } else {
            self.config_auto_apply(ws, user, &ds).await
        };
        let (text, meta) = config_reply(ask, &ds, &applied);
        reply(&text, None, meta);
        Ok(Logged { item: None })
    }

    /// “ST-n 进展怎样了？”: the item as it stands, without disturbing its session.
    fn status_text(&self, id: &str) -> Result<String> {
        let Some(i) = self.store().item(id)? else {
            return Ok(format!("没找到 {id}。"));
        };
        let mut s = format!("{}「{}」：{}。", i.id, i.title, status_label(&i.status));
        if !i.now.trim().is_empty() {
            s.push_str(&format!("现在：{}。", i.now.trim()));
        }
        if !i.plan.is_empty() {
            let done = i.plan.iter().filter(|p| p.done).count();
            s.push_str(&format!("计划完成 {done}/{} 步。", i.plan.len()));
        }
        if let Some(d) = self
            .store()
            .decisions_of_item(id)?
            .into_iter()
            .rfind(|d| d.kind == "accept" && d.status == "open")
        {
            let first = d.draft.split("\n\n").next().unwrap_or("");
            s.push_str(&format!("\n\n已经交付，等你验收：{}", js_head(first, 600)));
        }
        Ok(s)
    }

    /// A correction said in words: back into the conversation for the latest item made here in
    /// the last 10 minutes, or the latest answer made into an item. Only the latest one counts.
    async fn spoken_reroute(
        &self,
        ws: &str,
        mid: &str,
        text: &str,
        r: &Route,
        user: Option<&str>,
        reply: Reply<'_>,
    ) -> Result<Logged> {
        let th = self.store().store().thread();
        let msgs = self.store().messages(ws)?;
        if TO_INLINE.is_match(text) {
            let target = th.items(ws)?.into_iter().find(|t| {
                r.item.as_deref().is_none_or(|i| i == t.item_id)
                    && now() - t.created <= REROUTE_WINDOW
                    && self
                        .store()
                        .item(&t.item_id)
                        .ok()
                        .flatten()
                        .is_some_and(|i| !i.is_finished())
            });
            if let Some(t) = target
                && let Some(rm) = t.reply_msg.clone()
            {
                let h = self
                    .reroute(ws, &rm, super::RerouteTo::Inline, user)
                    .await?;
                reply(
                    &format!("好，{} 不做了，改在这里回答。", t.item_id),
                    None,
                    Some(json!({"reply_to": h.message_id})),
                );
                return Ok(Logged {
                    item: Some(t.item_id),
                });
            }
        } else {
            let target = msgs
                .iter()
                .rev()
                .filter(|m| m.id != mid)
                .take(10)
                .find(|m| {
                    m.author == "steve" && m.meta.get("chat").is_some_and(crate::gateway::truthy)
                });
            if let Some(m) = target.cloned() {
                let h = self
                    .reroute(ws, &m.id, super::RerouteTo::Item, user)
                    .await?;
                let item = h.item.map(|i| i.id);
                if let Some(i) = &item {
                    reply(&format!("好，已经做成 {i}。"), Some(i), None);
                }
                return Ok(Logged { item });
            }
        }
        reply("是哪一条？我没找到最近能改的那条。", None, None);
        Ok(Logged { item: None })
    }

    #[allow(clippy::too_many_arguments)]
    fn on_item(
        &self,
        ws: &str,
        mid: &str,
        text: &str,
        files: &[FileRef],
        user: Option<&str>,
        mentions: &[Mention],
        r: &Route,
        named: Option<crate::repo::Bot>,
        start: bool,
        reply: Reply<'_>,
        steps: &Mutex<Steps>,
    ) -> Result<Logged> {
        let db = self.store();
        /* “好，改吧” after an answer with an offer takes the offer: not a new routing, not a correction */
        if named.is_none()
            && confirms(text)
            && let Some(m) = self.open_offer_before(ws, mid)?
        {
            let h = self.offer_as(ws, &m.id, true, start, user)?;
            if let Some(rs) = lock(steps).route.as_mut() {
                rs["offer"] = json!(m.id);
            }
            self.store().store().thread().route_reply(
                mid,
                &h.message_id,
                h.item.as_ref().map(|i| i.id.as_str()),
            )?;
            return Ok(Logged {
                item: h.item.map(|i| i.id),
            });
        }
        if let (Some(b), Some(rs)) = (&named, lock(steps).route.as_mut()) {
            rs["picked"] = json!(b.id);
        }
        /* a follow-up like “按建议” becomes the proposal it confirms; the user's own words stay with it */
        let goal = if named.is_some() {
            AT.replace(text, "").to_string()
        } else if !r.goal.trim().is_empty() && r.goal.trim() != text.trim() {
            format!("{}\n\n（用户原话：{text}）", r.goal.trim())
        } else {
            text.to_string()
        };
        let mut made = vec![];
        for t in &r.new_tags {
            made.push(db.tag_create(ws, &t.name, &t.cat, &t.description, "Steve")?);
        }
        let mut tag_ids: Vec<String> = vec![];
        for id in r.tag_ids.iter().chain(made.iter().map(|t| &t.id)) {
            if !tag_ids.contains(id) {
                tag_ids.push(id.clone());
            }
        }
        let bot = named
            .as_ref()
            .map(|b| b.id.clone())
            .unwrap_or_else(|| r.bot.clone());
        if self.conv_of(mid) == super::MAIN {
            self.phase(ws, "item");
        }
        let i = Steps::begin(
            steps,
            "create_item",
            json!({"title": r.title, "bot": bot, "project": r.project, "tags": tag_ids, "files": files.len()}),
        );
        let res = self.thread_item(ThreadItem {
            ws,
            origin: mid,
            title: r.title.clone(),
            goal,
            bot: Some(bot),
            project: r.project.clone(),
            tags: tag_ids.clone(),
            files: files.to_vec(),
            from_question: r.signals.iter().any(|s| s == "question"),
            answer: None,
            start,
            by: user.map(String::from),
        });
        let item = Steps::end(
            steps,
            i,
            res,
            |i| json!({"item": i.id, "bot": i.bot_id, "project": i.project_id}),
        )?;
        /* what the item got: the category first, Steve's own pick when the router named none */
        let tag_ids = db.tags_of_item(&item.id)?;
        let tag_text = if tag_ids.is_empty() {
            String::new()
        } else {
            let mut names = vec![];
            for id in &tag_ids {
                let name = db.tag(id)?.map(|t| t.name).unwrap_or_default();
                names.push(format!(
                    "{name}{}",
                    if made.iter().any(|m| &m.id == id) {
                        "（新建）"
                    } else {
                        ""
                    }
                ));
            }
            format!(" 标签：{}。", names.join("、"))
        };
        /* the router's sentence, unless the code picked another bot than it named */
        let head = match &named {
            Some(b) => format!("按你指定的，交给{} Bot 了。", b.name),
            None if r.bot_from_project => {
                format!("好，交给{}了，做完请你验收。", self.bot_name(&item))
            }
            None if !r.reply.is_empty() => r.reply.clone(),
            None => "好，交给它了，做完请你验收。".into(),
        };
        let people = match self.item_people(ws, user, &item.id, text, mentions) {
            Some(n) => format!(" {n}。"),
            None => String::new(),
        };
        if let Some(id) = reply(&format!("{head}{tag_text}{people}"), Some(&item.id), None) {
            db.store().thread().item_reply(&item.id, &id)?;
            super::flowback::adopt_quiet(db, &item.id, &id);
            self.said(ws, &self.conv_of(&id), &id);
        }
        Ok(Logged {
            item: Some(item.id),
        })
    }
}

/// A settings request with Steve's last reply, which “按这个方案改” refers to: the drafts read the
/// proposal from it, and leave it alone when the request says what to change by itself.
fn with_earlier(text: &str, history: &[Said]) -> String {
    match history.iter().rev().find(|m| m.author != "me") {
        Some(m) if !m.text.trim().is_empty() => format!(
            "{text}\n\n（Steve 上一条回复如下。只有用户这句话沿用它时（如“按这个方案”“就按你说的改”）才照它改，否则忽略：\n{}）",
            js_head(m.text.trim(), 6000)
        ),
        _ => text.to_string(),
    }
}

/// How it is set now, a line per field (at most 20).
fn listed(d: &crate::changes::Drafted) -> String {
    let mut lines: Vec<String> = d
        .current
        .iter()
        .take(20)
        .map(|(k, v)| format!("- {k}：{v}"))
        .collect();
    if d.current.len() > 20 {
        lines.push(format!("- ……还有 {} 项", d.current.len() - 20));
    }
    lines.join("\n")
}

/// The reply to a configuration request: the drafted cards (`meta.proposal`, or `meta.proposals` when
/// there are several; those `applied` already marked so in `meta.cfg_state`), and how the rest is set now.
fn config_reply(
    ask: bool,
    ds: &[crate::changes::Drafted],
    applied: &[String],
) -> (String, Option<Value>) {
    if ask {
        let parts: Vec<String> = ds
            .iter()
            .map(|d| format!("「{}」现在是：\n{}", d.title, listed(d)))
            .collect();
        return (
            format!(
                "{}\n\n要改的话直接说改成什么，我起草好给你确认。",
                parts.join("\n\n")
            ),
            None,
        );
    }
    let drafted: Vec<&Value> = ds.iter().filter_map(|d| d.proposal.as_ref()).collect();
    let unchanged: Vec<String> = ds
        .iter()
        .filter(|d| d.proposal.is_none())
        .map(|d| {
            let now = listed(d);
            if now.is_empty() {
                format!("我没看出要改「{}」的哪一项，换个说法试试？", d.title)
            } else {
                format!(
                    "我没看出要改「{}」的哪一项，换个说法试试？现在是：\n{now}",
                    d.title
                )
            }
        })
        .collect();
    let is_applied = |p: &Value| {
        p["id"]
            .as_str()
            .is_some_and(|id| applied.iter().any(|a| a == id))
    };
    let titled = |pred: &dyn Fn(&Value) -> bool| -> Vec<&str> {
        ds.iter()
            .filter(|d| d.proposal.as_ref().is_some_and(pred))
            .map(|d| d.title.as_str())
            .collect()
    };
    let (done, todo) = (titled(&is_applied), titled(&|p| !is_applied(p)));
    let done_text = match done.as_slice() {
        [] => String::new(),
        [t] => format!("已经改好了「{t}」，不对的话在卡片上撤销："),
        many => format!(
            "已经改好了 {} 处（{}），不对的话在卡片上撤销：",
            many.len(),
            many.iter()
                .map(|t| format!("「{t}」"))
                .collect::<Vec<_>>()
                .join("")
        ),
    };
    let todo_text = match todo.as_slice() {
        [] => String::new(),
        [t] if done.is_empty() => format!("这是配置改动，我起草好了，确认后应用到「{t}」："),
        [t] => format!("「{t}」的改动起草好了，确认后应用："),
        many => format!(
            "这是配置改动，我起草好了 {} 处，确认后分别应用：",
            many.len()
        ),
    };
    let head = [done_text, todo_text]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let text = std::iter::once(head)
        .chain(unchanged)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut meta = match drafted.as_slice() {
        [] => None,
        [p] => Some(json!({"proposal": p})),
        many => Some(json!({"proposals": many})),
    };
    if let Some(m) = meta.as_mut().filter(|_| !applied.is_empty()) {
        m["cfg_state"] = Value::Object(
            applied
                .iter()
                .map(|id| (id.clone(), json!("applied")))
                .collect(),
        );
    }
    (text, meta)
}
