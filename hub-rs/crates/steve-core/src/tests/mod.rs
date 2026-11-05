//! State-machine tests with a fake runtime and a temp database. `lifecycle`, `inbox`, `supersede` and
//! `restart` port hub/test/*.test.ts case by case; the others cover the rest of the orchestrator.

mod changes;
mod conversations;
mod cycle;
mod flows;
mod holds;
mod i18n;
mod inbox;
mod kinds;
mod lane;
mod lane_drive;
mod lifecycle;
mod models;
mod module_undo;
mod modules;
mod new_cycle;
mod outbox;
mod queued;
mod release_cleanup;
mod release_outcome;
mod restart;
mod routing;
mod stall;
mod stats_marks;
mod supersede;
mod takeover;
mod thread;
mod workspace;

use crate::repo::{NewDecision, NewItem, NewSession, Release, WS};
use crate::testkit::Kit;

/// A fake release script: waits for `$STEVE_HOME/go`, then runs `then`. It gives up (exit 1) when its
/// `STEVE_HOME` is gone or after about 30 s, so one a test leaves behind can't poll forever.
pub fn fake_release_script(then: &str) -> String {
    format!(
        "n=0\nwhile [ ! -f \"$STEVE_HOME/go\" ]; do\n  [ -d \"$STEVE_HOME\" ] || exit 1\n  n=$((n+1)); [ $n -ge 600 ] && exit 1\n  sleep 0.05\ndone\n{then}"
    )
}

/// An item in 交付与验收 with an idle session and an open accept decision (lifecycle.test.ts `delivered`).
pub struct Delivered {
    pub id: String,
    pub session: String,
    pub decision: String,
}

impl Kit {
    pub fn project_with_release(&self) -> String {
        let script = self.home().join("release.sh");
        let rel = Release {
            kind: "script".into(),
            cmd: script.to_string_lossy().into(),
            done_when: None,
        };
        match self
            .db()
            .project_by_path(WS, &self.home().to_string_lossy())
            .unwrap()
        {
            Some(p) => p.id,
            None => {
                self.db()
                    .project_create(
                        WS,
                        "P",
                        "",
                        &self.home().to_string_lossy(),
                        "dir",
                        false,
                        Some("dev"),
                        Some(&rel),
                    )
                    .unwrap()
                    .id
            }
        }
    }

    pub fn item(&self, status: &str, stage: &str, project: Option<&str>) -> String {
        let id = self
            .db()
            .item_create(
                WS,
                &NewItem {
                    title: "t".into(),
                    goal: "g".into(),
                    bot_id: "dev".into(),
                    project_id: project.map(String::from),
                    status: Some(status.into()),
                    stage: Some(stage.into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .id;
        // an item made already finished is closed, as migration 027 records old ones
        let outcome = match status {
            "done" => Some("completed"),
            "drop" => Some("cancelled"),
            _ => None,
        };
        if let Some(o) = outcome {
            self.db()
                .exec(
                    "UPDATE items SET phase = 'closed', outcome = ?, closed_at = updated WHERE id = ?",
                    &[&o as &dyn rusqlite::ToSql, &id],
                )
                .unwrap();
        }
        id
    }

    pub fn session(&self, item: &str, status: &str) -> String {
        let s = self
            .db()
            .session_create(&NewSession {
                workspace_id: WS.into(),
                item_id: Some(item.into()),
                bot_id: Some("dev".into()),
                harness: "claude-code".into(),
                model: None,
                reasoning_effort: None,
                cwd: self.home().to_string_lossy().into(),
                title: "t".into(),
                token: format!("tok-{item}-{}", crate::util::uid("x")),
            })
            .unwrap();
        self.db()
            .session_patch(&s.id, crate::patch![status: status])
            .unwrap();
        s.id
    }

    pub fn decision(
        &self,
        item: &str,
        session: Option<&str>,
        kind: &str,
        subject_type: &str,
        options: &[&str],
    ) -> String {
        self.db()
            .decision_create(NewDecision {
                workspace_id: WS.into(),
                kind: kind.into(),
                subject_type: subject_type.into(),
                subject_id: session.unwrap_or(item).into(),
                item_id: Some(item.into()),
                session_id: session.map(String::from),
                question: "验收".into(),
                options: Some(options.iter().map(|s| s.to_string()).collect()),
                requested_by: "dev".into(),
                ..Default::default()
            })
            .unwrap()
            .id
    }

    pub fn delivered(&self, with_release: bool) -> Delivered {
        let project = with_release.then(|| self.project_with_release());
        let id = self.item("run", "交付与验收", project.as_deref());
        let home = self.home().to_string_lossy().to_string();
        self.db()
            .item_patch(
                &id,
                crate::patch![cwd: home.as_str(), branch: with_release.then_some("steve/x")],
            )
            .unwrap();
        let session = self.session(&id, "idle");
        let decision = self.decision(&id, Some(&session), "accept", "item", &["pass", "rework"]);
        // delivered = a completion request waiting for this acceptance, as `steve_deliver` leaves it
        self.core
            .tx()
            .lc_request_complete(
                &id,
                "dev",
                crate::cycle::Policy::Human,
                false,
                "交付，等你验收",
                Some(decision.clone()),
                vec![],
            )
            .unwrap();
        Delivered {
            id,
            session,
            decision,
        }
    }

    /// The user marks the item done (the human acceptance of a completion request).
    pub fn finish(&self, item: &str) -> crate::Result<crate::repo::Item> {
        self.core.item_action(item, crate::ItemAction::Finish, "me")
    }

    pub fn phase(&self, item: &str) -> String {
        self.db().item(item).unwrap().unwrap().phase
    }

    pub fn status(&self, item: &str) -> (String, String, String) {
        let i = self.db().item(item).unwrap().unwrap();
        (i.status, i.stage, i.now)
    }

    pub fn user_texts(&self, sid: &str) -> Vec<String> {
        self.db()
            .events_after(sid, 0)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "user")
            .map(|e| e.data["text"].as_str().unwrap_or("").to_string())
            .collect()
    }

    pub fn open_fails(&self, item: &str) -> Vec<String> {
        self.db()
            .decisions_of_item(item)
            .unwrap()
            .into_iter()
            .filter(|d| d.kind == "fail" && d.status == "open")
            .map(|d| d.question)
            .collect()
    }
}
