//! [`FakeChannel`]: a scripted [`ChannelAdapter`] for tests of the channel pipeline (C1–C6) without
//! a provider. Adapter id `fake_chan`, accounts in system `slack` (family `slack_user`). Spaces and
//! members are what the test puts in; sends and recalls are recorded.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::json;
use steve_flow::AdapterError;
use steve_modules::accounts::connections::{
    AccountInfo, AuthKind, ConnCx, ConnectionProvider, Owner, ProviderError,
};
use steve_modules::accounts::repo::{self as arepo, NewAccount};
use steve_modules::accounts::service::ensure_seeds;
use steve_modules::contacts::service::ensure_base;
use steve_modules::{ApiResult, Caller};
use steve_store::{Store, uid};

use crate::channel::{
    ChannelAdapter, ChannelFeatures, HandleRef, InboundMessage, MemberInfo, OutContent,
    ReactionKind, SendTarget, SpaceInfo, SpaceKind,
};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The scripted channel.
#[derive(Default)]
pub struct FakeChannel {
    spaces: Mutex<HashMap<String, (SpaceInfo, Vec<MemberInfo>)>>,
    /// Every send, in order, with the external id returned.
    pub sent: Mutex<Vec<(SendTarget, OutContent, String)>>,
    pub recalled: Mutex<Vec<String>>,
    /// `space_info` / `members` fail while set.
    pub fail_info: Mutex<bool>,
    /// `send` fails with this text while set (C6 tests).
    pub fail_send: Mutex<Option<String>>,
    /// Calls of `space_info` and `members`.
    pub info_calls: Mutex<u32>,
    pub member_calls: Mutex<u32>,
    /// Reactions on messages now: `(ext message id, kind, reaction id)`.
    pub reactions: Mutex<Vec<(String, ReactionKind, String)>>,
    /// Every reaction call in order: `+ack om_1`, `-r1 om_1`.
    pub reaction_log: Mutex<Vec<String>>,
    /// `add_reaction` / `remove_reaction` fail while set (best-effort tests).
    pub fail_reactions: Mutex<bool>,
    /// `add_reaction` waits this long first (the ack must not hold the reply up for long).
    pub reaction_delay: Mutex<Option<std::time::Duration>>,
}

impl FakeChannel {
    pub const ID: &'static str = "fake_chan";
    /// `systems.id` of its accounts and handles.
    pub const SYSTEM: &'static str = "slack";
    /// The bot's handle: `seed_connection` creates its account with it.
    pub const BOT: &'static str = "ubot";

    pub fn new() -> Self {
        Self::default()
    }

    /// A handle in the fake's system.
    pub fn handle(h: &str) -> HandleRef {
        HandleRef::new(Self::SYSTEM, h)
    }

    /// Declares a space (`space_info` answers it, `members` lists `members` as `(handle, name)`).
    pub fn space(
        &self,
        ext_id: &str,
        name: &str,
        kind: SpaceKind,
        has_external: bool,
        members: &[(&str, &str)],
    ) {
        let ms: Vec<MemberInfo> = members
            .iter()
            .map(|(h, n)| MemberInfo {
                handle: Self::handle(h),
                name: (*n).into(),
                role: "member".into(),
                external: false,
            })
            .collect();
        lock(&self.spaces).insert(
            ext_id.into(),
            (
                SpaceInfo {
                    name: name.into(),
                    kind,
                    member_count: ms.len() as i64,
                    has_external,
                },
                ms,
            ),
        );
    }

    /// A text message from `from` in space `space`.
    pub fn message(space: &str, id: &str, from: &str, text: &str) -> InboundMessage {
        let mut m = InboundMessage::text(space, id, Self::handle(from), text, steve_store::now());
        m.via = Some("fake".into());
        m.raw = json!({ "space": space, "id": id, "from": from, "text": text });
        m
    }

    /// A connection of this adapter in `ws` made by `caller.me`. `personal`: an account of their
    /// “我” (`mode=self`); otherwise the own organization's bot with handle [`Self::BOT`]. Returns
    /// `(connection_id, account_id)`.
    pub fn seed_connection(
        store: &Store,
        caller: &Caller,
        personal: Option<&str>,
    ) -> ApiResult<(String, String)> {
        store.transaction(|tx| {
            ensure_seeds(tx, &caller.ws)?;
            let me = ensure_base(tx, caller)?;
            let (party, kind, handle, name) = match personal {
                Some(h) => (me.id.clone(), "personal", h.to_string(), me.name.clone()),
                None => {
                    let org = steve_modules::contacts::repo::self_org(tx, &caller.ws)?
                        .ok_or_else(|| steve_modules::ApiError::not_found("组织"))?;
                    (org.id, "app_bot", Self::BOT.to_string(), "测试机器人".to_string())
                }
            };
            let a = arepo::insert(
                tx,
                &caller.ws,
                &NewAccount {
                    system_id: Self::SYSTEM.into(),
                    family: "slack_user".into(),
                    party_id: party,
                    kind: kind.into(),
                    handle: handle.clone(),
                    handle_display: handle,
                    display_name: name,
                    verified: true,
                    confidence: 1.0,
                    source: "connect".into(),
                    source_label: String::new(),
                },
            )?;
            let con = uid("con");
            tx.execute(
                "INSERT INTO connections (id, workspace_id, account_id, adapter, auth, caps, created_by, created, updated)
                 VALUES (?, ?, ?, ?, 'none', '[\"msg.receive\",\"msg.send\",\"msg.send_as_user\"]', ?, 1, 1)",
                rusqlite::params![con, caller.ws, a.id, Self::ID, caller.me],
            )?;
            Ok((con, a.id))
        })
    }

    fn info(&self, ext: &str) -> Result<(SpaceInfo, Vec<MemberInfo>), AdapterError> {
        if *lock(&self.fail_info) {
            return Err(ProviderError::Transient("假渠道暂时连不上".into()).into());
        }
        lock(&self.spaces)
            .get(ext)
            .cloned()
            .ok_or_else(|| AdapterError::Malformed(format!("没有这个空间：{ext}")))
    }
}

#[async_trait]
impl ConnectionProvider for FakeChannel {
    fn id(&self) -> &str {
        Self::ID
    }
    fn system(&self) -> &str {
        Self::SYSTEM
    }
    fn name(&self) -> &str {
        "假渠道"
    }
    fn caps(&self) -> &[&'static str] {
        &["msg.receive", "msg.send"]
    }
    fn auth(&self) -> AuthKind {
        AuthKind::None
    }
    async fn verify(&self, _: &ConnCx) -> Result<AccountInfo, ProviderError> {
        let mut a = AccountInfo::new(Self::BOT, "测试机器人");
        a.kind = "app_bot".into();
        a.owner = Owner::SelfOrg;
        Ok(a)
    }
}

#[async_trait]
impl ChannelAdapter for FakeChannel {
    fn features(&self) -> ChannelFeatures {
        ChannelFeatures {
            thread_reply: true,
            recall: true,
            members: true,
            reactions: true,
            ..ChannelFeatures::default()
        }
    }

    async fn space_info(&self, _: &ConnCx, ext_id: &str) -> Result<SpaceInfo, AdapterError> {
        *lock(&self.info_calls) += 1;
        Ok(self.info(ext_id)?.0)
    }

    async fn members(&self, _: &ConnCx, ext_id: &str) -> Result<Vec<MemberInfo>, AdapterError> {
        *lock(&self.member_calls) += 1;
        Ok(self.info(ext_id)?.1)
    }

    async fn send(
        &self,
        _: &ConnCx,
        to: &SendTarget,
        content: &OutContent,
    ) -> Result<String, AdapterError> {
        if let Some(e) = lock(&self.fail_send).clone() {
            return Err(ProviderError::Transient(e).into());
        }
        let mut sent = lock(&self.sent);
        // the same uuid twice sends once, like Feishu
        if let Some(u) = &content.uuid
            && let Some((_, _, id)) = sent.iter().find(|(_, c, _)| c.uuid.as_ref() == Some(u))
        {
            return Ok(id.clone());
        }
        let id = format!("fm_{}", sent.len() + 1);
        sent.push((to.clone(), content.clone(), id.clone()));
        Ok(id)
    }

    async fn recall(&self, _: &ConnCx, ext_msg_id: &str) -> Result<(), AdapterError> {
        lock(&self.recalled).push(ext_msg_id.into());
        Ok(())
    }

    async fn add_reaction(
        &self,
        _: &ConnCx,
        ext_msg_id: &str,
        kind: ReactionKind,
    ) -> Result<String, AdapterError> {
        let delay = *lock(&self.reaction_delay);
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        lock(&self.reaction_log).push(format!("+{} {ext_msg_id}", kind.code()));
        if *lock(&self.fail_reactions) {
            return Err(ProviderError::Transient("假渠道不收表情".into()).into());
        }
        let mut rs = lock(&self.reactions);
        let id = format!("r{}", lock(&self.reaction_log).len());
        rs.push((ext_msg_id.into(), kind, id.clone()));
        Ok(id)
    }

    async fn remove_reaction(
        &self,
        _: &ConnCx,
        ext_msg_id: &str,
        reaction_id: &str,
    ) -> Result<(), AdapterError> {
        lock(&self.reaction_log).push(format!("-{reaction_id} {ext_msg_id}"));
        if *lock(&self.fail_reactions) {
            return Err(ProviderError::Transient("假渠道不收表情".into()).into());
        }
        lock(&self.reactions).retain(|(m, _, r)| !(m == ext_msg_id && r == reaction_id));
        Ok(())
    }
}

impl FakeChannel {
    /// The reaction kinds on a message now.
    pub fn reactions_on(&self, ext_msg_id: &str) -> Vec<ReactionKind> {
        lock(&self.reactions)
            .iter()
            .filter(|(m, _, _)| m == ext_msg_id)
            .map(|(_, k, _)| *k)
            .collect()
    }
}
