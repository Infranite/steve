use crate::contract::Q;
use sea_query::{Expr, ExprTrait, Query};

use crate::{Result, Store};

pub fn ids_in_state(store: &Store, state: &str) -> Result<Vec<String>> {
    store.read(|r| {
        r.all(
            Query::select()
                .column("id")
                .from("personal_notifications")
                .and_where(Expr::col("state").eq(state)),
            |row| row.get(0),
        )
    })
}

/// Simulate a row from a newer producer/import, including topics rejected by the current schema.
pub fn seed_waiting(
    store: &Store,
    ws: &str,
    user: &str,
    topic: &str,
    at: i64,
    created: i64,
) -> Result<String> {
    let id = crate::uid("notification");
    store.write_locked(|c| {
        c.execute(
            "INSERT INTO personal_notifications
                (id,workspace_id,user_id,event_id,delivery_key,mode,body,state,next_attempt_at,created,topic)
             VALUES (?1,?2,?3,?1,?1,'now','body','waiting',?4,?5,'todo.due')",
            rusqlite::params![id, ws, user, at, created],
        )?;
        if topic != "todo.due" {
            c.pragma_update(None, "ignore_check_constraints", true)?;
            let result = c.execute(
                "UPDATE personal_notifications SET topic=? WHERE id=?",
                [topic, id.as_str()],
            );
            c.pragma_update(None, "ignore_check_constraints", false)?;
            result?;
        }
        Ok(id)
    })
}

#[derive(Debug, Clone, Copy)]
pub enum TargetRestriction {
    Group,
    ExtraMember,
    DetachedUser,
    DeletedParty,
    BotContact,
    BotParty,
    NoMembers,
    DisabledSender,
    BotSender,
    DisabledAccount,
    DisabledConnection,
    RevokedConnection,
    LeftSpace,
    LeftIdentity,
    Blocked,
    NoDisclosure,
    PersonalBot,
    BotContactAccount,
    DifferentOwner,
    ForeignMembership,
    ForeignSpace,
    PrivateParty,
    PrivateAccountParty,
    RemovedMember,
}

pub struct TargetFixture<'a> {
    pub ws: &'a str,
    pub other_ws: &'a str,
    pub user: &'a str,
    pub other_user: &'a str,
    pub space: &'a str,
    pub party: &'a str,
    pub connection: &'a str,
}

impl TargetFixture<'_> {
    pub fn restrict(&self, w: &mut crate::W<'_>, case: TargetRestriction) -> Result<()> {
        use TargetRestriction::*;
        let db = w.fixture_connection();
        match case {
            Group => {
                db.execute(
                    r#"UPDATE spaces SET kind='group',member_count=1 WHERE id=?"#,
                    [self.space],
                )?;
            }
            ExtraMember => {
                db.execute(r#"INSERT INTO space_members(workspace_id,space_id,account_id,joined) SELECT ?,?,id,1 FROM accounts WHERE handle='uhan'"#, [self.ws, self.space])?;
            }
            DetachedUser => {
                db.execute(
                    r#"UPDATE parties SET user_id=NULL WHERE id=?"#,
                    [self.party],
                )?;
            }
            DeletedParty => {
                db.execute(
                    r#"UPDATE parties SET status='deleted' WHERE id=?"#,
                    [self.party],
                )?;
            }
            BotContact => {
                db.execute(
                    r#"UPDATE parties SET contact_kind='bot' WHERE id=?"#,
                    [self.party],
                )?;
            }
            BotParty => {
                db.execute(r#"UPDATE parties SET type='bot' WHERE id=?"#, [self.party])?;
            }
            NoMembers => {
                db.execute(
                    r#"DELETE FROM space_members WHERE space_id=?"#,
                    [self.space],
                )?;
            }
            DisabledSender => {
                db.execute(
                    r#"UPDATE accounts SET status='disabled' WHERE handle='ulilei'"#,
                    [],
                )?;
            }
            BotSender => {
                db.execute(
                    r#"UPDATE accounts SET kind='app_bot' WHERE handle='ulilei'"#,
                    [],
                )?;
            }
            DisabledAccount => {
                db.execute(r#"UPDATE accounts SET status='disabled' WHERE id=(SELECT account_id FROM connections WHERE id=?)"#, [self.connection])?;
            }
            DisabledConnection => {
                db.execute(
                    r#"UPDATE connections SET status='disabled' WHERE id=?"#,
                    [self.connection],
                )?;
            }
            RevokedConnection => {
                db.execute(
                    r#"UPDATE connections SET status='revoked' WHERE id=?"#,
                    [self.connection],
                )?;
            }
            LeftSpace => {
                db.execute(
                    r#"UPDATE spaces SET status='left' WHERE id=?"#,
                    [self.space],
                )?;
            }
            LeftIdentity => {
                db.execute(
                    r#"UPDATE space_identities SET left_at=1 WHERE space_id=?"#,
                    [self.space],
                )?;
            }
            Blocked => {
                db.execute(r#"INSERT INTO space_prefs(workspace_id,space_id,attention,updated) VALUES(?,?,'blocked',1) ON CONFLICT(space_id) DO UPDATE SET attention='blocked'"#, [self.ws, self.space])?;
            }
            NoDisclosure => {
                db.execute(r#"INSERT INTO space_settings(workspace_id,space_id,disclosure,updated) VALUES(?,?,'never',1) ON CONFLICT(space_id) DO UPDATE SET disclosure='never'"#, [self.ws, self.space])?;
            }
            PersonalBot => {
                db.execute(
                    r#"UPDATE channel_settings SET mode='self' WHERE connection_id=?"#,
                    [self.connection],
                )?;
            }
            BotContactAccount => {
                db.execute(r#"UPDATE accounts SET party_id=? WHERE id=(SELECT account_id FROM connections WHERE id=?)"#, [self.party, self.connection])?;
                db.execute(
                    r#"UPDATE channel_settings SET mode='bot' WHERE connection_id=?"#,
                    [self.connection],
                )?;
            }
            DifferentOwner => {
                db.execute(r#"UPDATE parties SET user_id=? WHERE id=(SELECT a.party_id FROM accounts a JOIN connections c ON c.account_id=a.id WHERE c.id=?)"#, [self.other_user, self.connection])?;
            }
            ForeignMembership => {
                db.execute(
                    r#"UPDATE space_members SET workspace_id=? WHERE space_id=?"#,
                    [self.other_ws, self.space],
                )?;
            }
            ForeignSpace => {
                db.execute(
                    r#"UPDATE spaces SET workspace_id=? WHERE id=?"#,
                    [self.other_ws, self.space],
                )?;
            }
            PrivateParty => {
                db.execute(
                    r#"UPDATE parties SET visibility='private',created_by=? WHERE id=?"#,
                    [self.other_user, self.party],
                )?;
            }
            PrivateAccountParty => {
                db.execute(r#"UPDATE parties SET visibility='private',created_by=? WHERE id=(SELECT a.party_id FROM accounts a JOIN connections c ON c.account_id=a.id WHERE c.id=?)"#, [self.other_user, self.connection])?;
            }
            RemovedMember => {
                db.execute(
                    r#"DELETE FROM workspace_members WHERE workspace_id=? AND user_id=?"#,
                    [self.ws, self.user],
                )?;
            }
        }
        Ok(())
    }
}

pub struct DeliveryDeadline<'a> {
    pub id: &'a str,
    pub ws: &'a str,
    pub user: &'a str,
    pub now: i64,
}

impl DeliveryDeadline<'_> {
    pub fn seed_delivery(&self, w: &mut crate::W<'_>) -> Result<()> {
        w.fixture_connection().execute(
            "INSERT INTO output_deliveries(id,workspace_id,requester,idempotency_key,fingerprint,intent,connection_id,space_id,message_id,created)
             SELECT id,workspace_id,?2,id,'test','{}','test',space_id,id,?3 FROM chan_messages WHERE id=?1",
            rusqlite::params![self.id, self.user, self.now],
        )?;
        Ok(())
    }

    pub fn seed_notice(&self, w: &mut crate::W<'_>, notice: &str, delay: i64) -> Result<()> {
        w.fixture_connection().execute(
            "INSERT INTO personal_notifications(id,workspace_id,user_id,event_id,delivery_key,mode,body,state,next_attempt_at,created,delivery_id)
             VALUES (?1,?2,?3,?1,?1,'now','body','submitted',?4,?5,?6)",
            rusqlite::params![notice, self.ws, self.user, self.now + delay, self.now, self.id],
        )?;
        Ok(())
    }

    pub fn enable_policy(&self, w: &mut crate::W<'_>) -> Result<()> {
        w.fixture_connection().execute(
            "INSERT OR REPLACE INTO notification_policies
                (workspace_id,user_id,enabled,mode,timezone,consented_at,consent_since_seq,updated)
             VALUES (?, ?, 1, 'now', 'UTC', 0, 0, 0)",
            [self.ws, self.user],
        )?;
        Ok(())
    }

    pub fn mark_direct(&self, w: &mut crate::W<'_>, created: i64) -> Result<()> {
        w.fixture_connection().execute(
            "UPDATE chan_messages SET decision_id=NULL, created=?2, sent=NULL WHERE id=?1",
            rusqlite::params![self.id, created],
        )?;
        Ok(())
    }
}

pub fn set_unavailable(store: &Store, unavailable: bool) -> Result<()> {
    store.write_locked(|c| {
        let sql = if unavailable {
            "ALTER TABLE personal_notifications RENAME TO unavailable_notifications"
        } else {
            "ALTER TABLE unavailable_notifications RENAME TO personal_notifications"
        };
        c.execute_batch(sql)?;
        Ok(())
    })
}
