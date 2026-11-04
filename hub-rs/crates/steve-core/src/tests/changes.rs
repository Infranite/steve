//! Conversational configuration: drafts for bots, projects, settings and tags; apply and undo.

use crate::changes::Target;
use crate::error::msg;
use crate::repo::WS;
use crate::testkit::Kit;
use serde_json::json;

fn target(kind: &str, id: Option<&str>) -> Target {
    Target {
        kind: kind.into(),
        id: id.map(String::from),
    }
}

#[tokio::test]
async fn a_bot_draft_encodes_labels_and_undo_restores() {
    let k = Kit::new();
    k.gw.set(|p, _| {
        assert!(p.starts_with("You configure \"the bot \"Steve 开发\"\" in Steve"));
        assert!(p.contains("  accept_mode · 交付验收 · enum · 跟随全局|需要你验收|自动验收 · \"跟随全局\""));
        assert!(p.contains("  default_project · 默认项目 · enum · 无 · \"无\""));
        Ok(r#"{"changes":[{"key":"accept_mode","value":"自动验收"},{"key":"proactive","value":"积极"},{"key":"name","value":"Steve 开发"},{"key":"model","value":""},{"key":"bogus","value":1}]}"#.into())
    });
    let p = k
        .core
        .draft(WS, target("bot", Some("dev")), "以后自动验收，积极一点")
        .await
        .unwrap();
    assert_eq!(p.title, "Steve 开发 Bot 的设置");
    assert_eq!(p.href, "#/bots/dev");
    assert_eq!(
        p.patch,
        json!({"accept_mode": "auto", "proactive": 2})
            .as_object()
            .unwrap()
            .clone()
    );
    assert_eq!(
        serde_json::to_value(&p.changes).unwrap(),
        json!([{"label": "交付验收", "from": "跟随全局", "to": "自动验收"}, {"label": "主动程度", "from": "适中", "to": "积极"}])
    );
    let r = k.core.apply(&p.id, "me", WS).unwrap();
    assert_eq!(r, json!({"ok": true, "undo": p.id}));
    let b = k.db().bot("dev").unwrap().unwrap();
    assert_eq!((b.accept_mode.as_str(), b.proactive), ("auto", 2));
    assert_eq!(
        k.core.apply(&p.id, "me", WS).unwrap_err().to_string(),
        "这份草稿已经失效，请重新说一次"
    );
    assert_eq!(
        k.core.undo(&p.id, "me", "other").unwrap_err().to_string(),
        "这次改动已经不能撤销了"
    );
    k.core.undo(&p.id, "me", WS).unwrap();
    let b = k.db().bot("dev").unwrap().unwrap();
    assert_eq!((b.accept_mode.as_str(), b.proactive), ("inherit", 1));
    assert!(k.core.undo(&p.id, "me", WS).is_err());
}

#[tokio::test]
async fn a_draft_of_another_workspace_cannot_be_applied() {
    let k = Kit::new();
    k.gw.set(|_, _| Ok(r#"{"changes":[{"key":"persona","value":"热情"}]}"#.into()));
    let p = k
        .core
        .draft(WS, target("bot", Some("steve")), "热情点")
        .await
        .unwrap();
    assert!(k.core.apply(&p.id, "me", "other").is_err());
    assert!(
        k.core
            .draft(WS, target("bot", Some("ghost")), "x")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_project_draft() {
    let k = Kit::new();
    let pr = k
        .db()
        .project_create(WS, "Web", "旧说明", "/tmp", "git", false, None, None)
        .unwrap();
    k.gw.set(|_, _| Ok(r#"{"changes":[{"key":"default_bot","value":"Steve 开发"},{"key":"worktree","value":"开"},{"key":"description","value":"旧说明"}]}"#.into()));
    let p = k
        .core
        .draft(
            WS,
            target("project", Some(&pr.id)),
            "默认交给开发，开 worktree",
        )
        .await
        .unwrap();
    assert_eq!(p.title, "项目 Web");
    assert_eq!(
        p.patch,
        json!({"default_bot": "dev", "worktree": true})
            .as_object()
            .unwrap()
            .clone()
    );
    assert_eq!(
        serde_json::to_value(&p.changes).unwrap(),
        json!([{"label": "默认 Bot", "from": "无", "to": "Steve 开发"}, {"label": "每个事项单独开 worktree 分支", "from": "关", "to": "开"}])
    );
    k.core.apply(&p.id, "me", WS).unwrap();
    let x = k.db().project(Some(&pr.id)).unwrap().unwrap();
    assert_eq!((x.default_bot.as_deref(), x.worktree), (Some("dev"), true));
    k.core.undo(&p.id, "me", WS).unwrap();
    let x = k.db().project(Some(&pr.id)).unwrap().unwrap();
    assert_eq!((x.default_bot, x.worktree), (None, false));
}

#[tokio::test]
async fn a_settings_draft_with_the_item_prefix() {
    let k = Kit::new();
    k.gw.set(|p, _| {
        assert!(p.contains("  accDefault · 默认验收方式（值：manual=需要你验收，auto=自动验收） · enum · manual|auto · \"manual\""));
        assert!(p.contains("  item_prefix · 事项编号前缀（大写字母） · text · \"ST\""));
        Ok(r#"{"changes":[{"key":"accDefault","value":"auto"},{"key":"item_prefix","value":"ab-c1defgh"},{"key":"depsAfter","value":"随便"}]}"#.into())
    });
    let p = k
        .core
        .draft(WS, target("settings", Some("items")), "x")
        .await
        .unwrap();
    assert_eq!(
        p.patch,
        json!({"accDefault": "auto", "item_prefix": "ABCDEF"})
            .as_object()
            .unwrap()
            .clone()
    );
    assert_eq!(
        serde_json::to_value(&p.changes).unwrap(),
        json!([{"label": "默认验收方式", "from": "需要你验收", "to": "自动验收"}, {"label": "事项编号前缀（大写字母）", "from": "ST", "to": "ABCDEF"}])
    );
    k.core.apply(&p.id, "me", WS).unwrap();
    assert_eq!(k.db().setting(WS, "accDefault", "manual").unwrap(), "auto");
    assert_eq!(k.db().workspace(WS).unwrap().unwrap().item_prefix, "ABCDEF");
    k.core.undo(&p.id, "me", WS).unwrap();
    assert_eq!(k.db().setting(WS, "accDefault", "x").unwrap(), "manual");
    assert_eq!(k.db().workspace(WS).unwrap().unwrap().item_prefix, "ST");
}

#[tokio::test]
async fn tags_are_created_and_described_then_undone() {
    let k = Kit::new();
    let old = k.db().tag_create(WS, "财务", "领域", "钱", "你").unwrap();
    k.gw.set(|_, _| Ok(r#"{"create":[{"name":"健康","cat":"领域","description":"身体"},{"name":"坏","cat":"不存在","description":"x"},{"name":"财务","cat":"领域","description":"重复"}],"edit":[{"name":"财务","description":"钱和账"},{"name":"没有","description":"x"}]}"#.into()));
    let p = k
        .core
        .draft(WS, target("tags", None), "加个健康标签")
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&p.changes).unwrap(),
        json!([{"label": "新建标签 · 领域", "from": "—", "to": "健康：身体"}, {"label": "描述 · 财务", "from": "钱", "to": "钱和账"}])
    );
    k.core.apply(&p.id, "me", WS).unwrap();
    assert!(k.db().tag_by_name(WS, "健康").unwrap().is_some());
    assert_eq!(k.db().tag(&old.id).unwrap().unwrap().description, "钱和账");
    k.core.undo(&p.id, "me", WS).unwrap();
    assert!(k.db().tag_by_name(WS, "健康").unwrap().is_none());
    assert_eq!(k.db().tag(&old.id).unwrap().unwrap().description, "钱");
}

#[tokio::test]
async fn global_requests_pick_a_target_and_fall_back_to_general() {
    let k = Kit::new();
    k.gw.set(|p, _| {
        if p.starts_with("Which Steve configuration") {
            assert!(p.contains("bot:dev — Steve 开发（开发 Steve 自己：Hub、前端、部署脚本）"));
            Err(msg("down"))
        } else {
            Ok(r#"{"changes":[]}"#.into())
        }
    });
    let p = k
        .core
        .draft(WS, target("global", None), "随便")
        .await
        .unwrap();
    assert_eq!(p.title, "设置 · 通用");
    assert!(p.changes.is_empty());
    assert_eq!(
        serde_json::to_value(&p).unwrap()["target"],
        json!({"kind": "settings", "id": "general"})
    );
}

#[tokio::test]
async fn a_models_draft_sets_lists_effort_and_agent_defaults_and_undoes() {
    let k = Kit::new();
    k.gw.set(|p, _| {
        assert!(p.starts_with("You configure \"Steve settings · models\" in Steve"));
        assert!(p.contains("  route.harness · 分流（"));
        assert!(p.contains("跟随 Steve（现在是 Claude Code · claude-sonnet-5-5 → claude-opus-5-5）"));
        assert!(p.contains("  route · 分流 · 模型"));
        assert!(p.contains("  route.effort · 分流 · 推理强度 · enum · 默认|低|中|高|很高|最高|关闭|最低 · \"默认\""));
        assert!(p.contains("  agent.claude-code · Claude Code 新会话的默认模型"));
        Ok(r#"{"changes":[{"key":"route","value":"claude-sonnet-5-5，claude-haiku-5-5"},{"key":"chat.harness","value":"Codex"},{"key":"research.effort","value":"最高"},{"key":"agent.codex","value":"gpt-6-sol"},{"key":"yolo","value":"not a model!"}]}"#.into())
    });
    let p = k
        .core
        .draft(
            WS,
            target("models", None),
            "分流用 sonnet，不行再 haiku；对话用 Codex；研究想深一点；Codex 默认 gpt-6-sol",
        )
        .await
        .unwrap();
    assert_eq!(
        (p.title.as_str(), p.href.as_str()),
        ("设置 · 模型", "#/settings/models")
    );
    assert_eq!(
        serde_json::to_value(&p.changes).unwrap(),
        json!([
            {"label": "分流 · 由谁来做", "from": "Claude Code · claude-haiku-5-5 → claude-sonnet-5-5", "to": "Claude Code · claude-sonnet-5-5 → claude-haiku-5-5"},
            {"label": "对话回答 · 由谁来做", "from": "跟随 Steve", "to": "Codex · 默认模型"},
            {"label": "就地研究 · 由谁来做", "from": "跟随 Steve", "to": "Claude Code · 默认模型 · 推理最高"},
            {"label": "Codex 新会话默认模型", "from": "默认", "to": "gpt-6-sol"},
        ])
    );
    k.core.apply(&p.id, "me", WS).unwrap();
    let m = k.core.models();
    assert_eq!(
        m.for_purpose(WS, "route").models,
        ["claude-sonnet-5-5", "claude-haiku-5-5"]
    );
    assert_eq!(m.for_purpose(WS, "chat").harness, "codex");
    let r = m.for_purpose(WS, "research");
    assert_eq!(r.effort.as_deref(), Some("max"));
    assert_eq!(
        r.models,
        ["claude-sonnet-5-5", "claude-opus-5-5"],
        "Steve's models"
    );
    assert_eq!(m.agent_model(WS, "codex").as_deref(), Some("gpt-6-sol"));
    k.core.undo(&p.id, "me", WS).unwrap();
    assert_eq!(
        m.for_purpose(WS, "route").models,
        ["claude-haiku-5-5", "claude-sonnet-5-5"]
    );
    assert_eq!(m.for_purpose(WS, "chat").harness, "claude-code");
    assert_eq!(m.for_purpose(WS, "research").effort, None);
    assert!(m.custom(WS, "research").is_none());
    assert_eq!(m.agent_model(WS, "codex"), None);
    let n: i64 = k
        .db()
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM audit WHERE action = 'settings.models'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(n, 2, "apply and undo are both audited");
}

#[tokio::test]
async fn tags_are_renamed_moved_and_removed_only_when_unused_then_undone() {
    let k = Kit::new();
    let ui = k.db().tag_create(WS, "UI", "领域", "界面", "你").unwrap();
    k.db().tag_create(WS, "旧", "主题", "不用了", "你").unwrap();
    let used = k.db().tag_create(WS, "在用", "领域", "", "你").unwrap();
    let item = k.item("run", "x", None);
    k.db().set_item_tags(&item, std::slice::from_ref(&used.id)).unwrap();
    k.gw.set(|_, _| {
        Ok(r#"{"edit":[{"name":"UI","newName":"前端","cat":"类型"},{"name":"在用","newName":"旧"}],"remove":["旧","在用","没有"]}"#.into())
    });
    let p = k
        .core
        .draft(
            WS,
            target("tags", None),
            "UI 改名前端放到类型里，删掉旧的和在用的",
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&p.changes).unwrap(),
        json!([
            {"label": "改名 · UI", "from": "UI", "to": "前端"},
            {"label": "分类 · UI", "from": "领域", "to": "类型"},
            {"label": "删除标签", "from": "旧", "to": "—"},
            {"label": "删除 · 在用", "from": "在用", "to": "（没有删：还有 1 处在用，可以在标签页合并到别的标签）"}
        ]),
        "a rename onto an existing name is left out; a tag in use is not removed"
    );
    k.core.apply(&p.id, "me", WS).unwrap();
    let t = k.db().tag(&ui.id).unwrap().unwrap();
    assert_eq!((t.name.as_str(), t.cat.as_str()), ("前端", "类型"));
    assert!(k.db().tag_by_name(WS, "旧").unwrap().is_none());
    assert!(k.db().tag(&used.id).unwrap().is_some());
    k.core.undo(&p.id, "me", WS).unwrap();
    let t = k.db().tag(&ui.id).unwrap().unwrap();
    assert_eq!((t.name.as_str(), t.cat.as_str()), ("UI", "领域"));
    let back = k.db().tag_by_name(WS, "旧").unwrap().unwrap();
    assert_eq!(
        (back.cat.as_str(), back.description.as_str()),
        ("主题", "不用了")
    );
}

#[tokio::test]
async fn a_bot_is_switched_off_and_a_project_archived() {
    let k = Kit::new();
    let pr = k
        .db()
        .project_create(WS, "Web", "", "/tmp", "git", false, None, None)
        .unwrap();
    k.gw.set(|p, _| {
        if p.contains("the bot") {
            assert!(p.contains("  enabled · 启用状态 · enum · 启用|停用 · \"启用\""));
            Ok(r#"{"changes":[{"key":"enabled","value":"停用"}]}"#.into())
        } else {
            assert!(p.contains("  archived · 状态（归档后不再出现在项目列表和分派选项里） · enum · 使用中|已归档 · \"使用中\""));
            Ok(r#"{"changes":[{"key":"archived","value":"已归档"}]}"#.into())
        }
    });
    let b = k
        .core
        .draft(WS, target("bot", Some("dev")), "停用开发 Bot")
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&b.changes).unwrap(),
        json!([{"label": "启用状态", "from": "启用", "to": "停用"}])
    );
    k.core.apply(&b.id, "me", WS).unwrap();
    assert!(!k.db().bot("dev").unwrap().unwrap().enabled);
    let p = k
        .core
        .draft(WS, target("project", Some(&pr.id)), "归档 Web")
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&p.changes).unwrap(),
        json!([{"label": "状态", "from": "使用中", "to": "已归档"}])
    );
    k.core.apply(&p.id, "me", WS).unwrap();
    assert!(k.db().project(Some(&pr.id)).unwrap().unwrap().archived);
    k.core.undo(&p.id, "me", WS).unwrap();
    assert!(!k.db().project(Some(&pr.id)).unwrap().unwrap().archived);
    /* the main bot has no switch */
    let (_, _, now) = k.core.describe(WS, &target("bot", Some("steve"))).unwrap();
    assert!(!now.iter().any(|(l, _)| l == "启用状态"));
}

#[tokio::test]
async fn describe_shows_settings_as_the_user_sees_them() {
    let k = Kit::new();
    let (title, href, now) = k
        .core
        .describe(WS, &target("settings", Some("items")))
        .unwrap();
    assert_eq!(
        (title.as_str(), href.as_str()),
        ("设置 · 事项与验收", "#/settings/items")
    );
    assert_eq!(
        now,
        [
            ("默认验收方式", "需要你验收"),
            ("前置完成后", "自动开始"),
            ("交接阈值", "80%"),
            ("完成后自动归档", "30 天"),
            ("事项编号前缀", "ST"),
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
    );
    assert!(
        k.core
            .describe(WS, &target("settings", Some("nope")))
            .is_err()
    );
}
