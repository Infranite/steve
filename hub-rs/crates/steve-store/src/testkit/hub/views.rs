use crate::{NewDecision, Result, Store, StoreError, WS};
use steve_types::{ChannelsEvent, PeopleEvent};

fn owner_party_id(conn: &rusqlite::Connection) -> Result<String> {
    Ok(conn.query_row(
        "SELECT p.id FROM parties p
         JOIN workspace_members wm
           ON wm.workspace_id = p.workspace_id AND wm.user_id = p.user_id
         WHERE p.workspace_id = 'default' AND wm.role = 'owner'
         LIMIT 1",
        [],
        |row| row.get(0),
    )?)
}

pub fn seed_channel_tiers(store: &Store) -> Result<()> {
    store.write(|tx| -> Result<_, StoreError> {
        // Users::create links every workspace member to a contact party; reuse the owner's party.
        let owner = owner_party_id(tx.conn)?;
        let now = crate::now();
        let inbound_at = now - 2_000;
        let draft_at = now - 1_000;
        tx.conn.execute_batch(
            "INSERT INTO parties (id, workspace_id, type, name, created_by, created, updated)
               VALUES ('tier-star', 'default', 'person', 'A', 'test', 1, 1),
                      ('tier-quiet', 'default', 'person', 'B', 'test', 1, 1);",
        )?;
        tx.conn.execute(
            "INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
               VALUES ('tier-account', 'default', 'test', 'test', ?1, 'personal', 'me', 'me', 'test', 1, 1)",
            [&owner],
        )?;
        tx.conn.execute_batch(
            "INSERT INTO connections (id, workspace_id, account_id, adapter, auth, created_by, created, updated)
               VALUES ('tier-connection', 'default', 'tier-account', 'test', 'none', 'test', 1, 1);
             INSERT INTO spaces (id, workspace_id, system_id, kind, connection_id, external_id, name, created, updated)
               VALUES ('tier-space-a', 'default', 'test', 'dm', 'tier-connection', 'a', 'A', 1, 1),
                      ('tier-space-b', 'default', 'test', 'dm', 'tier-connection', 'b', 'B', 1, 1);",
        )?;
        tx.conn.execute(
            "INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, sender_party_id, text, created)
               VALUES ('tier-in-a', 'default', 'tier-space-a', 'a-1', 'in', 'tier-star', 'hello', ?1),
                      ('tier-in-b', 'default', 'tier-space-b', 'b-1', 'in', 'tier-quiet', 'hello', ?1)",
            [inbound_at],
        )?;
        tx.conn.execute(
            "INSERT INTO chan_messages (id, workspace_id, space_id, reply_to, direction, text, status, created)
               VALUES ('tier-out-a', 'default', 'tier-space-a', 'a-1', 'out', 'reply A', 'draft', ?1),
                      ('tier-out-b', 'default', 'tier-space-b', 'b-1', 'out', 'reply B', 'draft', ?1)",
            [draft_at],
        )?;
        Ok(())
    })
}

pub fn channel_decisions(store: &Store) -> Result<(String, String, String)> {
    store.write(|tx| -> Result<_, StoreError> {
        let mut decision = |kind: &str, message: &str| {
            crate::DecisionRows(tx)
                .open(&NewDecision {
                    workspace_id: WS.into(),
                    subject_type: "chan_message".into(),
                    subject_id: message.into(),
                    kind: kind.into(),
                    options: Some(vec!["no".into()]),
                    ..Default::default()
                })
                .map(|d| d.id)
        };
        let star = decision("approve", "tier-out-a")?;
        let quiet = decision("approve", "tier-out-b")?;
        let failed = decision("fail", "tier-out-b")?;
        for (id, message) in [(&star, "tier-out-a"), (&quiet, "tier-out-b")] {
            crate::channel_message_repo::outbound::link_decision(tx, message, id)?;
        }
        Ok((star, quiet, failed))
    })
}

pub fn star_and_reply(store: &Store) -> Result<()> {
    store.write(|tx| -> Result<_, StoreError> {
        let owner = owner_party_id(tx.conn)?;
        let reply_at = crate::now();
        tx.conn
            .execute("UPDATE parties SET important = 1 WHERE id = 'tier-star'", [])?;
        tx.emit(steve_types::DomainEvent::from(PeopleEvent::Party {
            id: "tier-star".into(),
            ws: WS.into(),
        }));
        tx.conn.execute(
            "INSERT INTO chan_messages (id, workspace_id, space_id, direction, sender_party_id, text, created)
               VALUES ('tier-reply', 'default', 'tier-space-b', 'in', ?1, 'I replied', ?2)",
            rusqlite::params![&owner, reply_at],
        )?;
        tx.emit(steve_types::DomainEvent::from(ChannelsEvent::ChanMessage {
            id: "tier-reply".into(),
            space_id: "tier-space-b".into(),
            ws: WS.into(),
        }));
        Ok(())
    })
}

pub fn remove_reply(store: &Store) -> Result<()> {
    store.write(|tx| -> Result<_, StoreError> {
        tx.conn
            .execute("DELETE FROM chan_messages WHERE id = 'tier-reply'", [])?;
        tx.emit(steve_types::DomainEvent::from(ChannelsEvent::ChanMessage {
            id: "tier-reply".into(),
            space_id: "tier-space-b".into(),
            ws: WS.into(),
        }));
        Ok(())
    })
}

pub fn reply_revisions(store: &Store) -> Result<Vec<(i64, serde_json::Value, String, i64)>> {
    store.read_conn(|r| crate::base::revisions(r, WS, "chan_message", "tier-out-b"))
}

pub fn silently_reopen_decision(store: &Store, id: &str) -> Result<()> {
    store.write(|w| {
        w.conn.execute(
            "UPDATE decisions SET status = 'open', resolved = NULL WHERE id = ?",
            [id],
        )?;
        Ok(())
    })
}
