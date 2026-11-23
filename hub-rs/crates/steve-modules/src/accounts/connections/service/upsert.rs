use super::*;

struct Upserting<'a, 'tx> {
    conns: &'a Connections,
    tx: &'a mut W<'tx>,
    viewer: &'a Viewer,
    actor: &'a str,
    provider: &'a dyn ConnectionProvider,
    system: &'a arepo::System,
    now: i64,
}

struct VerifiedConfig {
    config: Value,
    pair: Option<Value>,
    version: Option<Value>,
    health: HealthState,
    available: Vec<String>,
}

impl Connections {
    /// The account and connection rows for a verified credential. Returns the old credential item
    /// to destroy after the commit when an existing connection got the new one.
    pub(super) fn upsert_tx(
        &self,
        tx: &mut W<'_>,
        viewer: &Viewer,
        actor: &str,
        p: &dyn ConnectionProvider,
        mut u: Upsert,
    ) -> VaultResult<(Connected, Option<String>)> {
        let ws = viewer.ws.as_str();
        let now = self.now();
        let system = own_system(tx, ws, p.system())?;
        let h = normalize(&system, &u.info.handle).ok_or_else(|| {
            ApiError::rule(steve_i18n::t!("modules.connections.verify_no_account"))
        })?;
        // your own identity, proven by the provider: a contact a channel made for it joins “我” first
        if u.reauth_of.is_none() && u.info.owner == Owner::Me && p.claims_self() {
            self.claim_into_me(tx, viewer, actor, &system.id, &u.info)?;
        }
        let mut run = Upserting {
            conns: self,
            tx,
            viewer,
            actor,
            provider: p,
            system: &system,
            now,
        };
        let (account, personal_of) = run.account(&u, &h)?;
        let existing = run.existing(&account, personal_of, &u, &h)?;
        let pair = run.pair(existing.as_ref(), &mut u)?;
        let version = version_value(p.config_version());
        let available = p.available_caps(&u.scopes);
        let config = Upserting::config(&u, pair.clone(), version.clone());
        let health = HealthState {
            last_ok: now,
            expires_at: u.expires_at,
            refresh_expires_at: u.refresh_exp,
            ..Default::default()
        };
        let verified = VerifiedConfig {
            config,
            pair,
            version,
            health,
            available,
        };
        let (done, old) = match existing {
            Some(c) if u.fresh_only => {
                return Err(ApiError::conflict(steve_i18n::t!(
                    "modules.connections.app_connected",
                    id = c.id
                ))
                .into());
            }
            Some(c) => run.reconnect(c, &account, &u, verified)?,
            None => run.create(&account, &u, verified)?,
        };
        run.finish(&account, &done, &u)?;
        Ok((done, old))
    }
}

impl Upserting<'_, '_> {
    fn account(
        &mut self,
        u: &Upsert,
        h: &asvc::Handle,
    ) -> VaultResult<(arepo::Account, Option<String>)> {
        let tx = &mut *self.tx;
        let viewer = self.viewer;
        let actor = self.actor;
        let system = self.system;
        let ws = self.viewer.ws.as_str();
        let known = Accounts(tx).by_handle(ws, &system.family, &h.handle)?;
        let owner = match &u.info.owner {
            Owner::Me => tx.contacts().me(ws, actor)?,
            Owner::SelfOrg => tx.contacts().own_org(ws)?,
            Owner::Team(id) => tx
                .contacts()
                .party(id)?
                .filter(|t| t.kind == "team" && t.workspace_id == ws && t.status == "active"),
            // a new app bot gets its own bot party; a known one keeps its party
            Owner::Bot if known.is_none() => Some(stewards::new_bot_party(
                tx,
                ws,
                actor,
                if u.info.display_name.trim().is_empty() {
                    &h.display
                } else {
                    &u.info.display_name
                },
                "connect",
            )?),
            Owner::Bot => tx.contacts().own_org(ws)?,
        }
        .ok_or_else(|| ApiError::not_found("联系人"))?;
        let mut personal_of: Option<String> = None;
        let account = match known {
            Some(id) => {
                let a = Accounts(tx)
                    .get(&id)?
                    .ok_or_else(|| ApiError::not_found("账号"))?;
                let party = tx.contacts().current_party(&a.party_id)?;
                // Opt-in strict bot/self contracts never adopt a known account of the other
                // kind. Other providers retain their existing account-adoption behavior.
                let bot = party
                    .as_ref()
                    .is_some_and(|p| p.contact_kind.as_deref() == Some("bot"));
                if u.info.strict_identity_owner
                    && ((u.info.owner == Owner::Bot && !bot) || (u.info.owner == Owner::Me && bot))
                {
                    return Err(ApiError::rule(
                        "account identity kind differs from the verified credential",
                    )
                    .into());
                }
                if !party.as_ref().is_some_and(may_connect) {
                    return Err(asvc::taken(tx, viewer, &a.id)?.into());
                }
                // someone's own account (their “我”) is connected only by that person
                if let Some(p) = party.as_ref().filter(|p| p.is_me()) {
                    if p.user_id.as_deref() != Some(actor) {
                        return Err(asvc::taken(tx, viewer, &a.id)?.into());
                    }
                    personal_of = p.user_id.clone();
                }
                a
            }
            None => {
                if u.reauth_of.is_some() {
                    return Err(ApiError::rule(format!(
                        "这次授权的是另一个账号（{}），请用原来的账号重新授权",
                        h.display
                    ))
                    .into());
                }
                asvc::record(
                    tx,
                    viewer,
                    actor,
                    &Record {
                        system_id: system.id.clone(),
                        party_id: owner.id.clone(),
                        kind: u.info.kind.clone(),
                        handle: u.info.handle.clone(),
                        display_name: u.info.display_name.clone(),
                        verified: true,
                        confidence: 1.0,
                        source: "connect".into(),
                        source_label: default_label("connect").into(),
                    },
                )?
            }
        };
        Ok((account, personal_of))
    }

    fn existing(
        &mut self,
        account: &arepo::Account,
        personal_of: Option<String>,
        u: &Upsert,
        h: &asvc::Handle,
    ) -> VaultResult<Option<ConnRow>> {
        let tx = &mut *self.tx;
        let actor = self.actor;
        let existing = ConnectionRows(tx).of_account(&account.id)?;
        // …and a personal connection is replaced only by the person who made it
        if personal_of.is_some()
            && let Some(c) = &existing
            && c.created_by != actor
        {
            return Err(
                ApiError::conflict(steve_i18n::t!("modules.connections.other_member")).into(),
            );
        }
        if let Some(r) = &u.reauth_of
            && existing.as_ref().is_none_or(|c| &c.id != r)
        {
            return Err(ApiError::rule(steve_i18n::t!(
                "modules.connections.other_account",
                account = h.display
            ))
            .into());
        }
        Ok(existing)
    }

    fn pair(&mut self, existing: Option<&ConnRow>, u: &mut Upsert) -> VaultResult<Option<Value>> {
        let conns = self.conns;
        let tx = &mut *self.tx;
        let viewer = self.viewer;
        let p = self.provider;
        // the pair again, in the commit: it may have gone since the flow started. A connection that
        // is there keeps the pair it has now, not the copy its flow took at the start (a re-pair
        // made meanwhile stands): on a re-authorization the row's pair alone, on a new connect onto
        // it the flow's only when the row has none.
        let this = existing.map_or(u.id.clone(), |c| c.id.clone());
        let mut auto = true;
        if let Some(c) = existing {
            let current = c.config.get(PAIR_KEY).filter(|v| !v.is_null()).cloned();
            if current.is_some() || u.reauth_of.is_some() {
                put_key(&mut u.config, PAIR_KEY, current);
                // a pair dropped meanwhile is not picked again (unless it is required)
                auto = p.pairing().is_some_and(|x| x.required);
            }
        }
        conns.check_pair(tx, viewer, p, &mut u.config, Some(&this), auto)?;
        Ok(u.config.get(PAIR_KEY).cloned())
    }

    fn config(u: &Upsert, pair: Option<Value>, version: Option<Value>) -> Value {
        let mut config = u.config.clone();
        merge(&mut config, &u.info.config);
        // the checked pair and the current version, whatever the merges brought
        put_key(&mut config, PAIR_KEY, pair);
        put_key(&mut config, CONFIG_VERSION_KEY, version);
        config
    }

    fn reconnect(
        &mut self,
        mut c: ConnRow,
        account: &arepo::Account,
        u: &Upsert,
        verified: VerifiedConfig,
    ) -> VaultResult<(Connected, Option<String>)> {
        let VerifiedConfig {
            config,
            pair,
            version,
            health,
            available,
        } = verified;
        let conns = self.conns;
        let tx = &mut *self.tx;
        let actor = self.actor;
        let p = self.provider;
        let now = self.now;
        let ws = self.viewer.ws.as_str();
        if c.adapter != p.id() {
            return Err(ApiError::conflict(steve_i18n::t!(
                "modules.connections.connected_via",
                adapter = conns
                    .provider(&c.adapter)
                    .map(|x| x.name().to_string())
                    .unwrap_or(c.adapter.clone())
            ))
            .into());
        }
        let old = c.credential_item.replace(u.item.clone());
        c.status = "ok".into();
        c.caps = pick_caps(&available, &u.caps, &c.caps);
        c.granted_scopes = u.scopes.clone();
        merge(&mut c.config, &config);
        put_key(&mut c.config, PAIR_KEY, pair);
        put_key(&mut c.config, CONFIG_VERSION_KEY, version);
        c.health = health;
        ConnectionRows(tx).update(&c, now)?;
        // connected again: what it missed meanwhile is history, not news
        ConnectionRows(tx).set_enabled_at(&c.id, now)?;
        conns.settle_decision(tx, ws, &c.id, json!({"reauthorized": true}))?;
        tx.audit(
            ws,
            actor,
            "connection.reauth",
            &c.id,
            &json!({"adapter": c.adapter, "account": account.id}),
        )?;
        Ok((
            Connected {
                connection_id: c.id,
                account_id: account.id.clone(),
                reconnected: true,
            },
            old.filter(|o| o != &u.item),
        ))
    }

    fn create(
        &mut self,
        account: &arepo::Account,
        u: &Upsert,
        verified: VerifiedConfig,
    ) -> VaultResult<(Connected, Option<String>)> {
        let VerifiedConfig {
            config,
            health,
            available,
            ..
        } = verified;
        let tx = &mut *self.tx;
        let actor = self.actor;
        let p = self.provider;
        let now = self.now;
        let ws = self.viewer.ws.as_str();
        let r = ConnRow {
            id: u.id.clone(),
            workspace_id: ws.into(),
            account_id: account.id.clone(),
            adapter: p.id().into(),
            auth: p.auth().column().into(),
            granted_scopes: u.scopes.clone(),
            caps: pick_caps(&available, &u.caps, &[]),
            credential_item: Some(u.item.clone()),
            status: "ok".into(),
            health,
            config,
            last_sync: None,
            created_by: actor.into(),
            revision: 1,
            created: now,
            updated: now,
        };
        ConnectionRows(tx).insert(&r)?;
        tx.audit(
            ws,
            actor,
            "connection.create",
            &r.id,
            &json!({"adapter": r.adapter, "account": account.id, "caps": r.caps}),
        )?;
        Ok((
            Connected {
                connection_id: r.id,
                account_id: account.id.clone(),
                reconnected: false,
            },
            None,
        ))
    }

    fn finish(
        &mut self,
        account: &arepo::Account,
        done: &Connected,
        u: &Upsert,
    ) -> VaultResult<()> {
        let conns = self.conns;
        let tx = &mut *self.tx;
        let actor = self.actor;
        let system = self.system;
        let ws = self.viewer.ws.as_str();
        steve_store::vault_items::VaultItems(tx).link_account(&u.item, &account.id)?;
        conns.link_stewards(tx, ws, actor, &account, &done.connection_id, &u.info)?;
        for x in &u.info.extra_handles {
            if let Some(eh) = normalize(&system, x) {
                Accounts(tx).add_handle(
                    ws,
                    &system.family,
                    &eh.handle,
                    &account.id,
                    Some(&done.connection_id),
                )?;
            }
        }
        // the same identity in other systems: the owner's accounts too, when nobody holds them
        for (sys, handle) in &u.info.aliases {
            attach_handle(
                tx,
                ws,
                actor,
                &account.party_id,
                sys,
                handle,
                &[],
                &Attach {
                    kind: &u.info.kind,
                    display_name: &u.info.display_name,
                    source: "connect",
                    source_label: default_label("connect"),
                },
            )?;
        }
        tx.emit(steve_types::DomainEvent::from(
            steve_types::ChannelsEvent::Connection {
                id: {
                    let value: String = done.connection_id.clone();
                    steve_types::ConnectionId::from(value)
                },
                ws: {
                    let value: String = ws.into();
                    steve_types::WorkspaceId::from(value)
                },
            },
        ));
        tx.emit(steve_types::DomainEvent::from(
            steve_types::PeopleEvent::Account {
                id: {
                    let value: String = account.id.clone();
                    steve_types::AccountId::from(value)
                },
                ws: {
                    let value: String = ws.into();
                    steve_types::WorkspaceId::from(value)
                },
            },
        ));
        Ok(())
    }
}

fn pick_caps(available: &[String], req: &Option<Vec<String>>, current: &[String]) -> Vec<String> {
    match req {
        Some(r) => available
            .iter()
            .filter(|c| r.contains(c))
            .cloned()
            .collect(),
        None if current.is_empty() => available.to_vec(),
        None => available
            .iter()
            .filter(|c| current.contains(c))
            .cloned()
            .collect(),
    }
}
