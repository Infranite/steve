//! Isolated database fixtures for knowledge integration tests.

use crate::{Result, Store};
use rusqlite::{Connection, params};

/// An in-memory copy of the exact FTS table the knowledge index uses.
pub struct FtsFixture(Connection);

impl FtsFixture {
    pub fn new() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("CREATE VIRTUAL TABLE kb_fts USING fts5(
            seg, title, tokenize = 'unicode61 remove_diacritics 2', content = '', contentless_delete = 1)")?;
        Ok(Self(conn))
    }

    pub fn put(&self, id: i64, body_terms: &str, title_terms: &str) -> Result<()> {
        self.0.execute(
            "INSERT INTO kb_fts (rowid, seg, title) VALUES (?, ?, ?)",
            params![id, body_terms, title_terms],
        )?;
        Ok(())
    }

    pub fn search(&self, expr: &str) -> Result<Vec<i64>> {
        let mut q = self.0.prepare_cached(
            "SELECT rowid FROM kb_fts WHERE kb_fts MATCH ? ORDER BY bm25(kb_fts,1.0,2.0) LIMIT 50",
        )?;
        Ok(q.query_map([expr], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn remove(&self, id: i64) -> Result<()> {
        self.0.execute("DELETE FROM kb_fts WHERE rowid=?", [id])?;
        Ok(())
    }

    pub fn content(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .0
            .query_row("SELECT seg FROM kb_fts WHERE rowid=?", [id], |r| r.get(0))?)
    }
}

pub fn seed_delivery(s: &Store) -> Result<()> {
    s
        .write_locked(|c| {
            c.execute_batch(
                "INSERT INTO parties (id, workspace_id, type, name, aliases, created_by, created, updated) VALUES
                   ('ct_sarah', 'default', 'contact', 'Sarah Chen', '[\"Sarah\"]', 'usr_dev', 1, 1),
                   ('org_acme', 'default', 'org', 'Acme', '[]', 'usr_dev', 1, 1);
                 INSERT INTO items(id, workspace_id, no, title, phase, cwd, created, updated, outcome) VALUES ('ST-12', 'default', 12, '给 Acme 写续签邮件', 'closed', NULL, 1, 100, 'completed');
                 INSERT INTO item_goals (item_id, version, text, created) VALUES
                   ('ST-12', 1, '给 Acme 写续签邮件', 1), ('ST-12', 2, '给 Acme 写续签邮件，抄送 Sarah', 2);
                 INSERT INTO timeline (item_id, ts, actor, kind, text) VALUES
                   ('ST-12', 3, 'usr_dev', 'me', 'Sarah 一直喜欢简短的邮件，别超过五行'),
                   ('ST-12', 4, 'usr_dev', 'me', '验收通过');
                 INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, item_id, question, draft, options, status, result, requested_by, created) VALUES
                   ('dec_ask', 'default', 'ask', 'question', 'ses_1', 'ST-12', '用中文还是英文？', '', '[]', 'resolved', '{\"answer\":\"Acme 的邮件一律用英文\"}', 'bot', 5),
                   ('dec_acc', 'default', 'accept', 'item', 'ST-12', 'ST-12', '做完了，请验收', '写好了续签邮件草稿，五行以内', '[\"pass\",\"rework\"]', 'resolved', '{\"choice\":\"pass\"}', 'bot', 6);
                 INSERT INTO artifacts (id, item_id, kind, title, location, created) VALUES ('art_1', 'ST-12', 'doc', '邮件草稿', NULL, 7);
                 INSERT INTO audit (workspace_id, ts, actor, action, target, detail) VALUES ('default', 1, 'usr_dev', 'item.create', 'ST-12', '{}');",
            )?;
            Ok(())
        })
}

pub fn only_member(s: &Store, ws: &str, user: &str) -> Result<()> {
    s.write_locked(|c| {
        c.execute(
            "DELETE FROM workspace_members WHERE workspace_id=? AND user_id<>?",
            params![ws, user],
        )?;
        Ok(())
    })
}

pub fn set_delivery_stamp(s: &Store, at: i64) -> Result<()> {
    s.write_locked(|c| {
        c.execute("UPDATE items SET updated=? WHERE id='ST-12'", [at])?;
        Ok(())
    })
}

pub fn decision_audits(s: &Store) -> Result<i64> {
    s.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM audit WHERE action='decision.resolve'",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn set_memory_created(s: &Store, id: &str, at: i64) -> Result<()> {
    s.write_locked(|c| {
        c.execute("UPDATE mems SET created=? WHERE id=?", params![at, id])?;
        Ok(())
    })
}

pub fn seed_day_messages(s: &Store, start: i64) -> Result<()> {
    s.write_locked(|c| {
        c.execute_batch(&format!(
                "INSERT INTO messages (id, workspace_id, space, author, text, item_id, created) VALUES
                   ('msg_1', 'default', 'steve', 'me', '以后周五下午不安排部署', NULL, {a}),
                   ('msg_2', 'default', 'steve', 'steve', '好的，记住了：周五下午不部署。', NULL, {b}),
                   ('msg_3', 'default', 'other', 'me', '别的空间', NULL, {b});",
                a = start + 10 * 3600 * 1000,
                b = start + 10 * 3600 * 1000 + 60_000,
            ))?;
        Ok(())
    })
}

pub fn notice_text(s: &Store) -> Result<Vec<String>> {
    s.sqlite_read(|c| {
        let mut q = c.prepare_cached("SELECT text FROM thread_notices ORDER BY created ASC")?;
        Ok(q.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    })
}

pub fn clear_messages(s: &Store) -> Result<()> {
    s.write_locked(|c| {
        c.execute("DELETE FROM messages", [])?;
        Ok(())
    })
}

pub fn seed_chat_catchup(s: &Store, messages: &[(&str, i64)]) -> Result<()> {
    s.write(|tx| {
        tx.conn.execute("INSERT OR REPLACE INTO settings (workspace_id, key, value, updated) VALUES ('default','tz','\"UTC\"',0)",[])?;
        for (id, at) in messages {
            tx.conn.execute("INSERT INTO messages (id, workspace_id, space, author, text, created) VALUES (?, 'default', 'steve', 'me', '以后周五不部署', ?)",params![id,at])?;
        }
        Ok(())
    })
}

pub fn seed_prompt_item(
    s: &Store,
    ws: &str,
    id: &str,
    session: &str,
    title: &str,
    goal: &str,
    by: Option<&str>,
) -> Result<()> {
    s
            .write_locked(|c| {
                c.execute(
                    "INSERT INTO items(id, workspace_id, no, kind, title, phase, created, updated, requested_by) VALUES (?, ?, (SELECT COUNT(*) + 1 FROM items), 'task', ?, 'active', 1, 1, ?)",
                    params![id, ws, title, by],
                )?;
                c.execute(
                    "INSERT INTO item_goals (item_id, version, text, created) VALUES (?, 1, ?, 1)",
                    params![id, goal],
                )?;
                c.execute(
                    "INSERT INTO sessions (id, workspace_id, item_id, no, harness, cwd, status, created, updated) VALUES (?, ?, ?, 1, 'claude', '/tmp', 'idle', 1, 1)",
                    params![session, ws, id],
                )?;
                if let Some(u) = by {
                    c.execute(
                        "INSERT INTO audit (workspace_id, ts, actor, action, target) VALUES (?, 1, ?, 'item.create', ?)",
                        params![ws, u, id],
                    )?;
                }
                Ok(())
            })
}

pub fn empty_chat_reply(s: &Store) -> Result<()> {
    s.write_locked(|c| {
        c.execute("INSERT INTO messages (id, workspace_id, space, author, text, created) VALUES ('msg_1', 'w2', 'main', 'steve', '', 1)",[])?;
        Ok(())
    })
}

pub fn audit_item_creator(s: &Store, ws: &str, actor: &str, item: &str) -> Result<()> {
    s.write_locked(|c| {
        c.execute("INSERT INTO audit (workspace_id, ts, actor, action, target) VALUES (?, 1, ?, 'item.create', ?)",params![ws,actor,item])?;
        Ok(())
    })
}

pub fn revoke_member(s: &Store, ws: &str, user: &str) -> Result<()> {
    s.write_locked(|c| {
        c.execute(
            "DELETE FROM workspace_members WHERE workspace_id=? AND user_id=?",
            params![ws, user],
        )?;
        Ok(())
    })
}

pub fn remove_user_memberships(s: &Store, user: &str) -> Result<()> {
    s.write_locked(|c| {
        c.execute("DELETE FROM workspace_members WHERE user_id = ?", [user])?;
        Ok(())
    })
}

pub fn seed_sarah(s: &Store) -> Result<()> {
    s
        .write_locked(|c| {
            c.execute_batch(
                "INSERT INTO parties (id, workspace_id, type, name, created_by, created, updated) VALUES ('ct_sarah', 'default', 'contact', 'Sarah Chen', 'usr_dev', 1, 1);",
            )?;
            Ok(())
        })
}

pub fn pause_source(s: &Store, source: &str) -> Result<()> {
    s.write_locked(|c| {
        c.execute("UPDATE kb_sources SET paused = 1 WHERE id = ?", [source])?;
        Ok(())
    })
}

pub fn seed_bot_memory(s: &Store) -> Result<()> {
    s
        .write_locked(|c| {
            c.execute_batch(
                "INSERT INTO bots (id, workspace_id, name, harness, updated) VALUES ('bot_k1', 'default', '小运', 'claude', 1);
                 INSERT OR IGNORE INTO bot_workspaces (bot_id, workspace_id, created) VALUES ('bot_k1', 'default', 1);
                 INSERT INTO bot_memories (id, bot_id, workspace_id, text, source, created) VALUES ('mem_1', 'bot_k1', 'default', '发布前要先跑冒烟测试', 'chat', 1);",
            )?;
            Ok(())
        })
}

pub fn edit_bot_memory(s: &Store) -> Result<()> {
    s.write_locked(|c| {
        c.execute(
            "UPDATE bot_memories SET text = '发布前要先跑回归测试' WHERE id = 'mem_1'",
            [],
        )?;
        Ok(())
    })
}

pub fn clear_bot_memories(s: &Store) -> Result<()> {
    s.write_locked(|c| {
        c.execute("DELETE FROM bot_memories", [])?;
        Ok(())
    })
}

pub fn edit_doc_without_indexing(s: &Store, doc: &str) -> Result<()> {
    s.write_locked(|c| {
        c.execute("UPDATE kb_docs SET body = '第二版内容' WHERE id = ?", [doc])?;
        Ok(())
    })
}

pub fn seed_mail_threads(s: &Store) -> Result<()> {
    s.write_locked(|c| {
        c.execute_batch("INSERT INTO parties (id, workspace_id, type, name, user_id, created_by, created, updated)
            VALUES ('mail_owner', 'default', 'person', 'Mail owner', 'usr_other', 'usr_other', 1, 1);
            INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
            VALUES ('mail_owner_acc', 'default', 'test', 'other', 'mail_owner', 'personal', 'mail-owner', 'Mail owner', 'me', 1, 1);
            INSERT INTO connections (id, workspace_id, account_id, adapter, auth, created_by, created, updated)
            VALUES ('mail_conn', 'default', 'mail_owner_acc', 'fixture', 'none', 'usr_other', 1, 1);
            INSERT INTO mail_threads (id, workspace_id, connection_id, thread_key, subject, visibility, created_by, last_at, created, updated)
            VALUES ('private_mail', 'default', 'mail_conn', 'private', 'Secret mail subject', 'private', 'usr_other', 1, 1, 1),
                   ('shared_mail', 'default', 'mail_conn', 'shared', 'Shared mail subject', 'workspace', 'usr_other', 1, 1, 1),
                   ('foreign_mail', 'w2', 'mail_conn', 'foreign', 'Foreign mail subject', 'workspace', 'usr_other', 1, 1, 1);")?;
        Ok(())
    })
}

pub fn seed_scoped_mail_threads(s: &Store) -> Result<()> {
    s.write_locked(|c| {
        c.execute_batch("INSERT INTO parties (id, workspace_id, type, name, created_by, created, updated)
            VALUES ('mail_party', 'default', 'person', 'Mailbox', 'usr_other', 1, 1);
            INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
            VALUES ('mail_acc', 'default', 'test', 'other', 'mail_party', 'personal', 'mail', 'Mail', 'me', 1, 1);
            INSERT INTO connections (id, workspace_id, account_id, adapter, auth, created_by, created, updated)
            VALUES ('scoped_mail_conn', 'default', 'mail_acc', 'fixture', 'none', 'usr_other', 1, 1);
            INSERT INTO mail_threads (id, workspace_id, connection_id, thread_key, subject, visibility, created_by, last_at, created, updated)
            VALUES ('team_mail', 'default', 'scoped_mail_conn', 'team', 'Team subject', 'team:team1', 'usr_other', 1, 1, 1),
                   ('space_mail', 'default', 'scoped_mail_conn', 'space', 'Space subject', 'space:s1', 'usr_other', 1, 1, 1);")?;
        Ok(())
    })
}

pub fn stored_search_hits(s: &Store, user: &str) -> Result<(i64, String)> {
    s.sqlite_read(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*), MAX(hits) FROM kb_uses WHERE consumer_type = 'search' AND consumer_id = ?",
                [user],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
}

pub fn search_index_rows(s: &Store) -> Result<i64> {
    s.sqlite_read(|c| {
            Ok(c.query_row(
                "SELECT (SELECT COUNT(*) FROM kb_chunks) + (SELECT COUNT(*) FROM kb_fts WHERE kb_fts MATCH '\"九月\"')",
                [],
                |r| r.get(0),
            )?)
        })
}

pub fn memory_chunk_scope(s: &Store, id: &str) -> Result<(String, String)> {
    s.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT visibility, level FROM kb_chunks WHERE obj_type = 'mem' AND obj_id = ?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })
}

pub fn insert_upload(
    s: &Store,
    ws: &str,
    id: &str,
    sha: &str,
    name: &str,
    mime: &str,
    size: i64,
    actor: &str,
) -> Result<()> {
    s
        .write_locked(|c| {
            c.execute(
                "INSERT INTO files (id, workspace_id, sha256, name, mime, size, created_by, created) VALUES (?, ?, ?, ?, ?, ?, ?, 1)",
                params![id, ws, sha, name, mime, size, actor],
            )?;
            Ok(())
        })
}

pub fn seed_steward_subjects(s: &Store) -> Result<()> {
    s
        .write_locked(|c| {
            c.execute_batch(
                "INSERT INTO parties (id, workspace_id, type, name, aliases, created_by, created, updated) VALUES
                   ('ct_sarah', 'default', 'contact', 'Sarah Chen', '[\"Sarah\"]', 'usr_dev', 1, 1),
                   ('org_acme', 'default', 'org', 'Acme', '[]', 'usr_dev', 1, 1);
                 INSERT INTO items(id, workspace_id, no, title, phase, cwd, created, updated) VALUES ('ST-12', 'default', 12, '整理知识', 'active', NULL, 1, 100);",
            )?;
            Ok(())
        })
}

pub fn seed_steward_use(s: &Store, mem: &str) -> Result<()> {
    s
        .write_locked(|c| {
            c.execute(
                "INSERT INTO kb_uses (id, workspace_id, consumer_type, consumer_id, query, hits, created) VALUES ('ku_1', 'default', 'session', 'ses_1', '邮件', ?, 50)",
                [serde_json::json!([{"n": 1, "obj_type": "mem", "obj_id": mem, "level": "conclusion", "label": "偏好"}]).to_string()],
            )?;
            Ok(())
        })
}

pub fn seed_draft_org(s: &Store) -> Result<()> {
    s
        .write_locked(|c| {
            c.execute_batch(
                "INSERT INTO parties (id, workspace_id, type, name, aliases, created_by, created, updated) VALUES
                   ('org_acme', 'default', 'org', 'Acme', '[\"艾克米\"]', 'usr_dev', 1, 1);",
            )?;
            Ok(())
        })
}

pub fn memory_count(s: &Store) -> Result<i64> {
    s.sqlite_read(|c| Ok(c.query_row("SELECT COUNT(*) FROM mems", [], |r| r.get(0))?))
}

pub fn indexed_memory_count(s: &Store) -> Result<i64> {
    s.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM kb_chunks WHERE obj_type = 'mem'",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn indexed_docs(s: &Store, id: &str) -> Result<i64> {
    s.sqlite_read(|c| Ok(c.query_row("SELECT COUNT(*) FROM kb_docs WHERE source_id = ? AND status = 'active' AND indexed IS NOT NULL", [id], |r| r.get(0))?))
}

pub fn syncing_sources(s: &Store, id: &str) -> Result<i64> {
    s.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM kb_sources WHERE id = ? AND status = 'syncing'",
            [id],
            |r| r.get(0),
        )?)
    })
}

pub fn document_count(s: &Store, id: &str) -> Result<i64> {
    s.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM kb_docs WHERE source_id = ?",
            [id],
            |r| r.get(0),
        )?)
    })
}

pub fn folder_chunks(s: &Store) -> Result<i64> {
    s.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM kb_chunks WHERE source_kind = 'folder'",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn folder_sources(s: &Store) -> Result<i64> {
    s.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM kb_sources WHERE kind = 'folder'",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn never_chunks(s: &Store, id: &str) -> Result<i64> {
    s.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM kb_chunks WHERE obj_id = ? AND level = 'never'",
            [id],
            |r| r.get(0),
        )?)
    })
}

pub fn candidate_count(s: &Store, origin: &str) -> Result<i64> {
    s.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM mem_candidates WHERE origin_id = ?",
            [origin],
            |r| r.get(0),
        )?)
    })
}
