//! Queries and mutations used by the GitHub integration fixtures.

use crate::{Result, Store};
use rusqlite::OptionalExtension;
use rusqlite::types::{Value as SqlValue, ValueRef};
use serde_json::{Value, json};

pub trait FixtureValue {
    fn fixture_value(&self) -> Value;
}

impl<T: serde::Serialize> FixtureValue for T {
    fn fixture_value(&self) -> Value {
        serde_json::to_value(self).expect("fixture parameter")
    }
}

fn bind<'a>(values: &'a [&dyn FixtureValue]) -> impl Iterator<Item = SqlValue> + 'a {
    values.iter().map(|value| match value.fixture_value() {
        Value::Null => SqlValue::Null,
        Value::Number(n) => n.as_i64().map_or_else(
            || SqlValue::Real(n.as_f64().expect("number")),
            SqlValue::Integer,
        ),
        Value::Bool(b) => SqlValue::Integer(i64::from(b)),
        Value::String(s) => SqlValue::Text(s),
        value => SqlValue::Text(value.to_string()),
    })
}

#[derive(Clone, Copy, Debug)]
pub enum Inspect {
    ConnectionsId,
    GhAppsSlug,
    GhAppsOwner,
    GhAppsCredentialItem,
    VaultFieldsGroupConcatLabelSecretValuePlainValueCt,
    ConnectionsConfig,
    ConnectionsCaps,
    ConnectionsAHandleAKindPOrgKind,
    ConnectionsCredentialItem,
    VaultFieldsGroupConcatLabelValuePlain,
    ConnectionsCount,
    GhAppsCount,
    ConnectionsStatus,
    DecisionsCount,
    ConnectionsAHandleAKindPUserId,
    ConnectionsJsonExtractConfigApiBase,
    ConnectionsId2,
    SecretUsesCount,
    SecretUsesCount2,
    GhPrsCount,
    GhDeliveriesCount,
    GhDeliveriesStatusRepoIdWorkspaceId,
    GhPrsOpenedByStateHeadRefChecksState,
    GhReposLastDelivery,
    GhPrsChecksState,
    GhChecksCount,
    GhChecksKindStatusConclusion,
    GhPrsHeadShaChecksState,
    GhPrsReviewState,
    GhIssuesCount,
    GhPrsState,
    GhReposEnabled,
    GhDeliveriesStatusError,
    GhDeliveriesStatus,
    RevisionsCount,
    GhPrsTitleOpenedBy,
    GhReposCount,
    GhReposPollState,
    GhDeliveryInputsStatusCount,
    GhDeliveriesCount2,
    GhPrsState2,
    GhDeliveriesStatus2,
    GhDeliveriesDStatusIAttempts,
    GhDeliveriesDStatusIAttemptsDError,
    GhPrsCount2,
    GhDeliveriesCount3,
    GhPrsCoalesceItemIdCoalesceLinkSource,
    LinksCount,
    ArtifactsTitle,
    TimelineActorKindText,
    TimelineCount,
    GhPrsId,
    GhPrsItemIdLinkSourceOpenedByHeadSha,
    ArtifactsTitleLocation,
    TimelineText,
    GhPrsNumber,
    GhPrsUrl,
    DecisionsCount2,
    GhPrsHeadShaChecksState2,
    FlowsNameOriginEnabled,
    GhEffectsStateAttempts,
    GhEffectsCount,
    GhReposConnectionId,
    DecisionsStatusJsonExtractResultChoiceJsonExtractResult,
    TimelineCount2,
    GhReposPrivate,
    GhPrsItemId,
    SessionsCount,
    GhReposId,
    GhChecksCount2,
    DecisionsOptionsRequestedBySessionIdItemId,
    GrantsCapabilitySubjectTypeSubjectIdResourceTypeDecisionId,
    GrantsUntilCreated,
    AuditCount,
    GhPrsId2,
    GhPrsUrl2,
    AuditCount2,
    GhPrsNumber2,
    ItemsStatus,
    GhEffectsState,
    GhEffectsStateNextAt,
    AuditCount3,
    GrantsCount,
    GrantsCount2,
    LinksCount2,
}

impl Inspect {
    fn statement(self) -> &'static str {
        match self {
            Self::ConnectionsId => "SELECT id FROM connections WHERE adapter = ?",
            Self::GhAppsSlug => "SELECT slug FROM gh_apps WHERE id = ?",
            Self::GhAppsOwner => "SELECT owner FROM gh_apps WHERE id = ?",
            Self::GhAppsCredentialItem => "SELECT credential_item FROM gh_apps WHERE id = ?",
            Self::VaultFieldsGroupConcatLabelSecretValuePlainValueCt => {
                "SELECT group_concat(label || ':' || secret || ':' || (value_plain IS NULL) || ':' || (value_ct IS NOT NULL), ',') FROM vault_fields WHERE item_id = ?"
            }
            Self::ConnectionsConfig => "SELECT config FROM connections WHERE id = ?",
            Self::ConnectionsCaps => "SELECT caps FROM connections WHERE id = ?",
            Self::ConnectionsAHandleAKindPOrgKind => {
                "SELECT a.handle || '|' || a.kind || '|' || p.org_kind FROM connections c JOIN accounts a ON a.id = c.account_id JOIN parties p ON p.id = a.party_id WHERE c.id = ?"
            }
            Self::ConnectionsCredentialItem => {
                "SELECT credential_item FROM connections WHERE id = ?"
            }
            Self::VaultFieldsGroupConcatLabelValuePlain => {
                "SELECT group_concat(label || ':' || (value_plain IS NULL), ',') FROM vault_fields WHERE item_id = ?"
            }
            Self::ConnectionsCount => "SELECT count(*) FROM connections WHERE adapter = ?",
            Self::GhAppsCount => "SELECT count(*) FROM gh_apps WHERE workspace_id = ?",
            Self::ConnectionsStatus => "SELECT status FROM connections WHERE id = ?",
            Self::DecisionsCount => {
                "SELECT count(*) FROM decisions WHERE subject_type = 'connection' AND subject_id = ?"
            }
            Self::ConnectionsAHandleAKindPUserId => {
                "SELECT a.handle || '|' || a.kind || '|' || p.user_id FROM connections c JOIN accounts a ON a.id = c.account_id JOIN parties p ON p.id = a.party_id WHERE c.id = ?"
            }
            Self::ConnectionsJsonExtractConfigApiBase => {
                "SELECT json_extract(config, '$.api_base') FROM connections WHERE id = ?"
            }
            Self::ConnectionsId2 => "SELECT id FROM connections WHERE adapter = 'github_app'",
            Self::SecretUsesCount => {
                "SELECT count(*) FROM secret_uses WHERE actor_id LIKE 'ghapp_%'"
            }
            Self::SecretUsesCount2 => "SELECT count(*) FROM secret_uses",
            Self::GhPrsCount => "SELECT count(*) FROM gh_prs",
            Self::GhDeliveriesCount => "SELECT count(*) FROM gh_deliveries WHERE delivery_id = ?",
            Self::GhDeliveriesStatusRepoIdWorkspaceId => {
                "SELECT status || '|' || repo_id || '|' || workspace_id FROM gh_deliveries WHERE delivery_id = ?"
            }
            Self::GhPrsOpenedByStateHeadRefChecksState => {
                "SELECT opened_by || '|' || state || '|' || head_ref || '|' || checks_state FROM gh_prs"
            }
            Self::GhReposLastDelivery => "SELECT last_delivery FROM gh_repos WHERE id = ?",
            Self::GhPrsChecksState => "SELECT checks_state FROM gh_prs WHERE number = 12",
            Self::GhChecksCount => {
                "SELECT count(*) FROM gh_checks WHERE head_sha = ? AND job_id IS NOT NULL AND run_id IS NOT NULL"
            }
            Self::GhChecksKindStatusConclusion => {
                "SELECT kind || '|' || status || '|' || conclusion FROM gh_checks WHERE ext_id = 'ci/legacy'"
            }
            Self::GhPrsHeadShaChecksState => {
                "SELECT head_sha || '|' || checks_state FROM gh_prs WHERE number = 12"
            }
            Self::GhPrsReviewState => "SELECT review_state FROM gh_prs WHERE number = 12",
            Self::GhIssuesCount => "SELECT count(*) FROM gh_issues",
            Self::GhPrsState => "SELECT state FROM gh_prs WHERE number = 12",
            Self::GhReposEnabled => "SELECT enabled FROM gh_repos WHERE id = ?",
            Self::GhDeliveriesStatusError => {
                "SELECT status || '|' || error FROM gh_deliveries WHERE delivery_id = ?"
            }
            Self::GhDeliveriesStatus => "SELECT status FROM gh_deliveries WHERE delivery_id = ?",
            Self::RevisionsCount => {
                "SELECT count(*) FROM revisions WHERE object_type = 'gh_repo' AND object_id = ?"
            }
            Self::GhPrsTitleOpenedBy => "SELECT title || '|' || opened_by FROM gh_prs",
            Self::GhReposCount => "SELECT count(*) FROM gh_repos",
            Self::GhReposPollState => "SELECT poll_state FROM gh_repos WHERE id = ?",
            Self::GhDeliveryInputsStatusCount => {
                "SELECT status || '|' || (SELECT count(*) FROM gh_delivery_inputs WHERE delivery_id = 'd-interrupted') FROM gh_deliveries WHERE delivery_id = 'd-interrupted'"
            }
            Self::GhDeliveriesCount2 => {
                "SELECT count(*) FROM gh_deliveries WHERE delivery_id = 'd-old'"
            }
            Self::GhPrsState2 => "SELECT state FROM gh_prs WHERE number = ?",
            Self::GhDeliveriesStatus2 => {
                "SELECT status FROM gh_deliveries WHERE delivery_id = 'd-late'"
            }
            Self::GhDeliveriesDStatusIAttempts => {
                "SELECT d.status || '|' || i.attempts FROM gh_deliveries d JOIN gh_delivery_inputs i USING (delivery_id) WHERE delivery_id = ?"
            }
            Self::GhDeliveriesDStatusIAttemptsDError => {
                "SELECT d.status || '|' || i.attempts || '|' || d.error FROM gh_deliveries d JOIN gh_delivery_inputs i USING (delivery_id) WHERE delivery_id = ?"
            }
            Self::GhPrsCount2 => "SELECT COUNT(*) FROM gh_prs WHERE repo_id = ?",
            Self::GhDeliveriesCount3 => {
                "SELECT COUNT(*) FROM gh_deliveries WHERE repo_id = ? AND event = 'pull_request'"
            }
            Self::GhPrsCoalesceItemIdCoalesceLinkSource => {
                "SELECT coalesce(item_id, '-') || '|' || coalesce(link_source, '-') FROM gh_prs WHERE number = ?"
            }
            Self::LinksCount => {
                "SELECT count(*) FROM links WHERE subject_type = 'item' AND subject_id = ? AND predicate = 'output' AND object_type = 'gh_pr'"
            }
            Self::ArtifactsTitle => "SELECT title FROM artifacts WHERE item_id = ? AND kind = 'PR'",
            Self::TimelineActorKindText => {
                "SELECT actor || ' ' || kind || ' ' || text FROM timeline WHERE item_id = ?"
            }
            Self::TimelineCount => "SELECT count(*) FROM timeline WHERE item_id IN (?, ?)",
            Self::GhPrsId => "SELECT id FROM gh_prs WHERE number = ?",
            Self::GhPrsItemIdLinkSourceOpenedByHeadSha => {
                "SELECT item_id || '|' || link_source || '|' || opened_by || '|' || head_sha FROM gh_prs WHERE number = ?"
            }
            Self::ArtifactsTitleLocation => {
                "SELECT title || ' ' || location FROM artifacts WHERE item_id = ? AND kind = 'PR'"
            }
            Self::TimelineText => {
                "SELECT text FROM timeline WHERE item_id = ? ORDER BY id DESC LIMIT 1"
            }
            Self::GhPrsNumber => "SELECT number FROM gh_prs WHERE opened_by = 'steve'",
            Self::GhPrsUrl => "SELECT url FROM gh_prs WHERE opened_by = 'steve'",
            Self::DecisionsCount2 => {
                "SELECT count(*) FROM decisions WHERE subject_type = 'gh_pr_merge'"
            }
            Self::GhPrsHeadShaChecksState2 => {
                "SELECT head_sha || '|' || checks_state FROM gh_prs WHERE number = ?"
            }
            Self::FlowsNameOriginEnabled => {
                "SELECT name || '|' || origin || '|' || enabled FROM flows WHERE origin = 'template:github'"
            }
            Self::GhEffectsStateAttempts => {
                "SELECT state || '|' || attempts FROM gh_effects WHERE decision_id = ?"
            }
            Self::GhEffectsCount => "SELECT count(*) FROM gh_effects WHERE decision_id = ?",
            Self::GhReposConnectionId => "SELECT connection_id FROM gh_repos LIMIT 1",
            Self::DecisionsStatusJsonExtractResultChoiceJsonExtractResult => {
                "SELECT status || ' ' || json_extract(result, '$.choice') || ' ' || json_extract(result, '$.auto') FROM decisions WHERE subject_type = 'gh_pr_open'"
            }
            Self::TimelineCount2 => {
                "SELECT count(*) FROM timeline WHERE item_id = ? AND text LIKE '验收后自动开草稿 PR%'"
            }
            Self::GhReposPrivate => "SELECT private FROM gh_repos",
            Self::GhPrsItemId => "SELECT item_id FROM gh_prs WHERE number = ?",
            Self::SessionsCount => "SELECT count(*) FROM sessions WHERE item_id = ?",
            Self::GhReposId => "SELECT id FROM gh_repos",
            Self::GhChecksCount2 => {
                "SELECT count(*) FROM gh_checks WHERE summary LIKE '%exit code%'"
            }
            Self::DecisionsOptionsRequestedBySessionIdItemId => {
                "SELECT options || ' ' || requested_by || ' ' || session_id || ' ' || item_id FROM decisions WHERE id = ?"
            }
            Self::GrantsCapabilitySubjectTypeSubjectIdResourceTypeDecisionId => {
                "SELECT capability || ' ' || subject_type || ' ' || subject_id || ' ' || resource_type || ' ' || decision_id FROM grants WHERE id = ?"
            }
            Self::GrantsUntilCreated => {
                "SELECT (until - created) / 3600000 FROM grants WHERE id = ?"
            }
            Self::AuditCount => "SELECT count(*) FROM audit WHERE action = 'github.comment'",
            Self::GhPrsId2 => "SELECT id FROM gh_prs WHERE item_id = ?",
            Self::GhPrsUrl2 => "SELECT url FROM gh_prs WHERE item_id = ?",
            Self::AuditCount2 => "SELECT count(*) FROM audit WHERE action = 'github.rerun'",
            Self::GhPrsNumber2 => "SELECT number FROM gh_prs WHERE item_id = ?",
            Self::ItemsStatus => "SELECT status FROM items WHERE id = ?",
            Self::GhEffectsState => "SELECT state FROM gh_effects WHERE decision_id = ?",
            Self::GhEffectsStateNextAt => {
                "SELECT state || '|' || (next_at IS NULL) FROM gh_effects WHERE decision_id = ?"
            }
            Self::AuditCount3 => {
                "SELECT count(*) FROM audit WHERE action = 'grant.add' AND target = ?"
            }
            Self::GrantsCount => {
                "SELECT count(*) FROM grants WHERE capability = 'github.comment' AND revoked IS NULL"
            }
            Self::GrantsCount2 => "SELECT count(*) FROM grants",
            Self::LinksCount2 => {
                "SELECT count(*) FROM links WHERE subject_type = 'item' AND subject_id = ? AND predicate = 'from' AND object_type = 'gh_issue' AND object_id = ?"
            }
        }
    }
    pub fn read(self, store: &Store, args: &[&dyn FixtureValue]) -> Result<Value> {
        store.sqlite_read(|c| {
            Ok(c.query_row(
                self.statement(),
                rusqlite::params_from_iter(bind(args)),
                |r| {
                    Ok(match r.get_ref(0)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(i) => json!(i),
                        ValueRef::Real(f) => json!(f),
                        ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                        ValueRef::Blob(_) => json!("<blob>"),
                    })
                },
            )?)
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Seed {
    RemoveVaultKey,
    ClearSecretUses,
    UpdateGhPrsItemId,
    InsertProjects,
    UpdateGhPrsItemId2,
    FailPrWrites,
    AllowPrWrites,
    UpdateGhReposPollState,
    FailPollMidway,
    AllowPollWrites,
    InsertProjects2,
    InsertBots,
    InsertItems,
    UpdateProjectsRelease,
    UpdateGhEffectsState,
    UpdateConnectionsStatus,
    UpdateGhReposPrivate,
    UpdateConnectionsAdapter,
    InsertProjects3,
    InsertBots2,
    InsertBotWorkspaces,
    UpdateConnectionsStatus2,
    InsertProjects4,
    InsertBots3,
    ItemsIdWorkspaceIdNoTitleStatusStage,
    GrantsUntil,
    ItemsStatusWaitId,
}

impl Seed {
    fn statement(self) -> &'static str {
        match self {
            Self::RemoveVaultKey => "DELETE FROM vault_keys WHERE workspace_id = ?",
            Self::ClearSecretUses => "DELETE FROM secret_uses",
            Self::UpdateGhPrsItemId => "UPDATE gh_prs SET item_id = 'ST-12' WHERE number = 12",
            Self::InsertProjects => "INSERT INTO projects (id, workspace_id, name, path, kind, created, updated) VALUES ('prj_demo', 'default', 'Demo', ?, 'git', 1, 1), ('prj_two', 'default', 'Two', '/nonexistent', 'git', 1, 1)",
            Self::UpdateGhPrsItemId2 => "UPDATE gh_prs SET item_id = 'ST-12'",
            Self::FailPrWrites => "CREATE TRIGGER no_prs BEFORE INSERT ON gh_prs BEGIN SELECT RAISE(ABORT, 'test store fault'); END",
            Self::AllowPrWrites => "DROP TRIGGER no_prs",
            Self::UpdateGhReposPollState => "UPDATE gh_repos SET poll_state = json_remove(poll_state, '$.pulls_since') WHERE id = ?",
            Self::FailPollMidway => "CREATE TRIGGER fail_cursor_delivery BEFORE INSERT ON gh_deliveries WHEN NEW.event = 'pull_request' AND (SELECT COUNT(*) FROM gh_deliveries WHERE event = 'pull_request') >= 60 BEGIN SELECT RAISE(FAIL, 'injected receipt failure'); END;",
            Self::AllowPollWrites => "DROP TRIGGER fail_cursor_delivery",
            Self::InsertProjects2 => "INSERT INTO projects (id, workspace_id, name, path, kind, created, updated) VALUES (?, ?, 'Demo', ?, 'git', 1, 1)",
            Self::InsertBots => "INSERT INTO bots (id, workspace_id, name, main, harness, enabled, updated)
            VALUES ('steve', 'default', 'Steve', 1, 'claude-code', 1, 1);
            INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES ('steve', 'default', 1);",
            Self::InsertItems => "INSERT INTO items (id, workspace_id, no, title, status, stage, cwd, branch, project_id, created, updated)
                 VALUES (?, ?, ?, '保留 return_to 参数', 'wait', '交付与验收', ?, ?, ?, 1, 1)",
            Self::UpdateProjectsRelease => "UPDATE projects SET release = './release.sh' WHERE id = ?",
            Self::UpdateGhEffectsState => "UPDATE gh_effects SET state = 'running' WHERE decision_id = ?",
            Self::UpdateConnectionsStatus => "UPDATE connections SET status = ? WHERE id = ?",
            Self::UpdateGhReposPrivate => "UPDATE gh_repos SET private = 0",
            Self::UpdateConnectionsAdapter => "UPDATE connections SET adapter = 'github_pat'",
            Self::InsertProjects3 => "INSERT INTO projects (id, workspace_id, name, path, kind, created, updated) VALUES ('prj_other', ?, 'Other', '/tmp/none', 'dir', 1, 1)",
            Self::InsertBots2 => "INSERT INTO bots (id, workspace_id, name, harness, updated) VALUES (?, ?, ?, 'claude', 1)",
            Self::InsertBotWorkspaces => "INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES (?, ?, 1)",
            Self::UpdateConnectionsStatus2 => "UPDATE connections SET status = ?, caps = ? WHERE id = ?",
            Self::InsertProjects4 => "INSERT INTO projects (id, workspace_id, name, path, kind, created, updated) VALUES (?, ?, ?, '/tmp/none', 'dir', 1, 1)",
            Self::InsertBots3 => "INSERT INTO bots (id, workspace_id, name, harness, main, updated) VALUES (?, ?, ?, 'claude', ?, 1)",
            Self::ItemsIdWorkspaceIdNoTitleStatusStage => "INSERT INTO items (id, workspace_id, no, title, status, stage, cwd, branch, project_id, created, updated)
         VALUES (?, ?, ?, '保留 return_to 参数', 'run', '执行', ?, ?, ?, 1, 1)",
            Self::GrantsUntil => "UPDATE grants SET until = 1",
            Self::ItemsStatusWaitId => "UPDATE items SET status = 'wait' WHERE id = ?",
        }
    }
    pub fn write(self, store: &Store, args: &[&dyn FixtureValue]) -> Result<usize> {
        store.write(|w| self.apply(w, args))
    }
    pub fn apply(self, w: &mut crate::W<'_>, args: &[&dyn FixtureValue]) -> Result<usize> {
        let tx = w.conn;
        match self {
            Self::FailPrWrites
            | Self::AllowPrWrites
            | Self::FailPollMidway
            | Self::AllowPollWrites
            | Self::InsertBots => {
                tx.execute_batch(self.statement())?;
                Ok(0)
            }
            _ => Ok(tx.execute(self.statement(), rusqlite::params_from_iter(bind(args)))?),
        }
    }
}

pub fn app_connection(store: &Store) -> Result<String> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT id FROM connections WHERE adapter = 'github_app'",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn open_decision(store: &Store, subject: &str) -> Result<Option<(String, String, String)>> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT id, question, draft FROM decisions WHERE subject_type = ? AND status = 'open'",
            [subject],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?)
    })
}

pub fn table_counts(store: &Store) -> Result<std::collections::BTreeMap<String, i64>> {
    store.sqlite_read(|c| {
        let names = c
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            )?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        names
            .into_iter()
            .map(|name| {
                let table = name.replace('"', "\"\"");
                let count = c.query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |r| {
                    r.get(0)
                })?;
                Ok((name, count))
            })
            .collect()
    })
}
