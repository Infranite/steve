use super::*;

#[derive(Default)]
struct Answers {
    asked: HashMap<String, Vec<(String, Outcome)>>,
    profiles: Vec<(Profile, Option<String>, String)>,
}

struct Lookup<'a> {
    channels: &'a Channels,
    ws: &'a str,
    now: i64,
    ids: &'a [Identity],
    open: &'a [Candidate],
    tenants: &'a HashMap<String, Vec<TenantInfo>>,
    own_keys: Vec<String>,
}

pub(super) async fn run(channels: &Channels, ws: &str) -> ChannelResult<NameReport> {
    let mut rep = NameReport::default();
    let store = channels.store().clone();
    let now = channels.now();
    let open = local_candidates(channels, ws, now, &mut rep).await?;
    let adapters = channels.profile_adapter_ids();
    let ids = if adapters.is_empty() {
        Vec::new()
    } else {
        store.reader(|c| identities(c, ws, &adapters))?
    };
    let tenants = identity_tenants(channels, &ids, now, &mut rep).await;
    link_local_tenants(channels, ws, &tenants, &mut rep).await?;
    if ids.is_empty() || open.is_empty() {
        rep.nameless = channels.count_nameless(ws)?;
        return Ok(rep);
    }
    let (w, o) = (ws.to_string(), open.clone());
    let spaces = channels
        .modules()
        .blocking(move |m| m.store.reader(|c| seen_spaces(c, &w, &o)))
        .await?;
    let mut answers = Answers::default();
    let mut lookup = Lookup {
        channels,
        ws,
        now,
        ids: &ids,
        open: &open,
        tenants: &tenants,
        own_keys: Vec::new(),
    };
    lookup.members(&spaces, &mut answers, &mut rep).await?;
    lookup.own_keys = tenants.values().flatten().map(|t| t.key.clone()).collect();
    let requests = lookup.directory_requests(&spaces, &answers)?;
    lookup.directory(requests, &mut answers, &mut rep).await;
    lookup.apply(&answers, &mut rep);
    lookup.note(&mut answers);
    rep.nameless = channels.count_nameless(ws)?;
    Ok(rep)
}

async fn local_candidates(
    channels: &Channels,
    ws: &str,
    now: i64,
    rep: &mut NameReport,
) -> ChannelResult<Vec<Candidate>> {
    // The big reads (whole-workspace scans of messages and payloads) run off the runtime.
    let w = ws.to_string();
    let lookups = channels.profile_systems();
    let cands = channels
        .modules()
        .blocking(move |m| m.store.read(|r| Ok(candidates(r, &w, now, &lookups)))?)
        .await?;
    rep.candidates = cands.len();

    // 2. what Steve already has (only looked at for the candidates)
    let (mentions, dms) = if cands.is_empty() {
        Default::default()
    } else {
        let w = ws.to_string();
        channels
            .modules()
            .blocking(move |m| {
                m.store.reader(|c| -> ChannelResult<_> {
                    Ok((mention_names(c, &w)?, dm_names(c, &w)?))
                })
            })
            .await?
    };
    let mut open: Vec<Candidate> = Vec::new();
    for cand in cands {
        let local = mentions
            .get(&cand.account_id)
            .or_else(|| dms.get(&cand.account_id));
        match local {
            Some(n) if !cand.named => {
                let e = Enrichment {
                    name: Some(n.clone()),
                    ..Enrichment::default()
                };
                if let Some(r) = channels.apply(ws, &cand, &e, rep)
                    && (r.account_named || r.renamed)
                {
                    rep.named_local += 1;
                }
                // still ask the provider for the organization of an unconfirmed person
                open.push(Candidate {
                    named: true,
                    ..cand
                });
            }
            _ => open.push(cand),
        }
    }
    Ok(open)
}

async fn identity_tenants(
    channels: &Channels,
    ids: &[Identity],
    now: i64,
    rep: &mut NameReport,
) -> HashMap<String, Vec<TenantInfo>> {
    let mut tenants: HashMap<String, Vec<TenantInfo>> = HashMap::new(); // installation → own tenants
    // the organizations of the identities
    for i in ids {
        let known = lock(&channels.names().mem)
            .tenants
            .get(&i.id)
            .filter(|(_, at)| now - at < MEMBERS_TTL_MS)
            .map(|(t, _)| t.clone());
        let t = match known {
            Some(t) => t,
            None => {
                let ans = channels
                    .ask(
                        i,
                        ProfileQuery {
                            space_ext_id: None,
                            handles: Vec::new(),
                            want_tenant: true,
                        },
                        rep,
                    )
                    .await;
                let t = ans.and_then(|a| a.tenant);
                lock(&channels.names().mem)
                    .tenants
                    .insert(i.id.clone(), (t.clone(), now));
                t
            }
        };
        if let Some(t) = t {
            tenants.entry(i.installation.clone()).or_default().push(t);
        }
    }
    tenants
}

async fn link_local_tenants(
    channels: &Channels,
    ws: &str,
    tenants: &HashMap<String, Vec<TenantInfo>>,
    rep: &mut NameReport,
) -> ChannelResult<()> {
    let store = channels.store().clone();
    // 2c. organizations from the stored payloads (anyone without a job yet, named or not)
    let w = ws.to_string();
    let (tenants_seen, mut own_keys) = channels
        .modules()
        .blocking(move |m| {
            m.store.reader(|c| -> ChannelResult<_> {
                Ok((local_tenants(c, &w)?, own_tenants(c, &w)?))
            })
        })
        .await?;
    // parties given an organization in this run: another account of theirs checks again
    let mut given: HashSet<String> = HashSet::new();
    let own_names: HashMap<String, String> = tenants
        .values()
        .flatten()
        .filter_map(|t| t.name.clone().map(|n| (t.key.clone(), n)))
        .collect();
    for t in tenants.values().flatten() {
        if !own_keys.contains(&t.key) {
            own_keys.push(t.key.clone());
        }
    }
    if !own_keys.is_empty() {
        for (acc, seen) in &tenants_seen {
            let tk = &seen.tenant;
            let hint = TenantHint {
                key: tk.clone(),
                own: own_keys.contains(tk),
                name: own_names.get(tk).cloned(),
                seen_in: Some(seen.space.clone()),
            };
            let Some(actor) = seen.actor.clone() else {
                continue;
            };
            // local_tenants left out whoever had a job; only a party given one just now
            // needs the check again
            if given.contains(&seen.party) && has_job(&store, ws, acc)? {
                continue;
            }
            given.insert(seen.party.clone());
            let ws2 = ws.to_string();
            let org = match store.write(|tx| accounts::tenant_org(tx, &ws2, &actor, &hint)) {
                Ok(o) => o,
                Err(e) => {
                    tracing::warn!(ws, "names: organization not recorded: {e}");
                    continue;
                }
            };
            let cand = Candidate {
                account_id: acc.clone(),
                actor,
                handle: HandleRef::new("", ""),
                named: true,
                retry: false,
            };
            let e = Enrichment {
                org_id: Some(org),
                ..Enrichment::default()
            };
            channels.apply(ws, &cand, &e, rep);
        }
    }
    Ok(())
}

impl Lookup<'_> {
    async fn members(
        &self,
        spaces: &[SeenSpace],
        answers: &mut Answers,
        rep: &mut NameReport,
    ) -> ChannelResult<()> {
        for sp in spaces.iter().take(MAX_SPACES) {
            if sp
                .accounts
                .iter()
                .all(|a| named_by(&answers.profiles, a, self.open))
            {
                continue;
            }
            self.space_members(sp, answers, rep).await?;
        }
        Ok(())
    }

    async fn space_members(
        &self,
        sp: &SeenSpace,
        answers: &mut Answers,
        rep: &mut NameReport,
    ) -> ChannelResult<()> {
        let (channels, now, ids, open) = (self.channels, self.now, self.ids, self.open);
        let store = channels.store();
        let inst = store.reader(|c| installation_of(ids, c, &sp.connection))?;
        let handles: Vec<HandleRef> = open
            .iter()
            .filter(|c| sp.accounts.contains(&c.account_id))
            .map(|c| c.handle.clone())
            .collect();
        for i in of_installation(ids, &inst) {
            let key = (i.id.clone(), sp.ext_id.clone());
            let fresh = lock(&channels.names().mem)
                .members
                .get(&key)
                .is_some_and(|at| now - at < MEMBERS_TTL_MS);
            let retry = open
                .iter()
                .any(|c| c.retry && sp.accounts.contains(&c.account_id));
            if fresh && !retry {
                continue;
            }
            let ans = channels
                .ask(
                    i,
                    ProfileQuery {
                        space_ext_id: Some(sp.ext_id.clone()),
                        handles: handles.clone(),
                        want_tenant: false,
                    },
                    rep,
                )
                .await;
            let o = Outcome::of(ans.as_ref());
            for c in open.iter().filter(|c| sp.accounts.contains(&c.account_id)) {
                answers
                    .asked
                    .entry(c.account_id.clone())
                    .or_default()
                    .push((i.id.clone(), o));
            }
            let Some(ans) = ans else {
                continue;
            };
            lock(&channels.names().mem).members.insert(key, now);
            let got = !ans.profiles.is_empty();
            for p in ans.profiles {
                answers
                    .profiles
                    .push((p, Some(sp.name.clone()), inst.clone()));
            }
            if got
                && sp
                    .accounts
                    .iter()
                    .all(|a| named_by(&answers.profiles, a, open))
            {
                break;
            }
        }
        Ok(())
    }

    fn directory_requests(
        &self,
        spaces: &[SeenSpace],
        answers: &Answers,
    ) -> ChannelResult<BTreeMap<String, Vec<HandleRef>>> {
        let (channels, ids, open) = (self.channels, self.ids, self.open);
        let store = channels.store();
        let mut by_inst: BTreeMap<String, Vec<HandleRef>> = BTreeMap::new();
        for cand in open {
            // named by the directory, or of another organization (whose directory is not ours): done
            if match_at(just_profiles(&answers.profiles), &cand.handle).is_some_and(|(_, p)| {
                !p.name.is_empty()
                    && (p.directory
                        || p.tenant_key.as_ref().is_some_and(|k| {
                            !self.own_keys.is_empty() && !self.own_keys.contains(k)
                        }))
            }) {
                continue;
            }
            // the installation of where they were seen (or every one)
            let insts: BTreeSet<String> = {
                let mut s = BTreeSet::new();
                for sp in spaces
                    .iter()
                    .filter(|s| s.accounts.contains(&cand.account_id))
                {
                    s.insert(store.reader(|c| installation_of(ids, c, &sp.connection))?);
                }
                if s.is_empty() {
                    s.extend(ids.iter().map(|i| i.installation.clone()));
                }
                s
            };
            for inst in insts {
                by_inst.entry(inst).or_default().push(cand.handle.clone());
            }
        }
        Ok(by_inst)
    }

    async fn directory(
        &self,
        by_inst: BTreeMap<String, Vec<HandleRef>>,
        answers: &mut Answers,
        rep: &mut NameReport,
    ) {
        let (channels, now, ids, open) = (self.channels, self.now, self.ids, self.open);
        for (inst, handles) in by_inst {
            let mut left = handles;
            for i in of_installation(ids, &inst) {
                left.retain(|h| {
                    let retry = open.iter().any(|c| c.retry && c.handle == *h);
                    let m = lock(&channels.names().mem);
                    retry
                        || m.handles
                            .get(&(i.id.clone(), h.handle.clone()))
                            .is_none_or(|at| now - at >= HANDLE_TTL_MS)
                });
                if left.is_empty() {
                    break;
                }
                let ans = channels
                    .ask(
                        i,
                        ProfileQuery {
                            space_ext_id: None,
                            handles: left.clone(),
                            want_tenant: false,
                        },
                        rep,
                    )
                    .await;
                let o = Outcome::of(ans.as_ref());
                for c in open.iter().filter(|c| left.contains(&c.handle)) {
                    answers
                        .asked
                        .entry(c.account_id.clone())
                        .or_default()
                        .push((i.id.clone(), o));
                }
                let Some(ans) = ans else {
                    continue;
                };
                {
                    let mut m = lock(&channels.names().mem);
                    for h in &left {
                        m.handles.insert((i.id.clone(), h.handle.clone()), now);
                    }
                }
                let found: Vec<Profile> = ans.profiles;
                left.retain(|h| match_profile(&found, h).is_none_or(|p| p.name.is_empty()));
                for p in found {
                    answers.profiles.push((p, None, inst.clone()));
                }
            }
        }
    }

    fn apply(&self, answers: &Answers, rep: &mut NameReport) {
        let (channels, ws, open) = (self.channels, self.ws, self.open);
        let store = channels.store();
        let tenants = self.tenants;
        // 4. apply
        for cand in open {
            let Some((at, p)) = match_at(just_profiles(&answers.profiles), &cand.handle) else {
                continue;
            };
            let (seen_in, inst) = (
                answers.profiles[at].1.clone(),
                answers.profiles[at].2.clone(),
            );
            let own_keys: Vec<TenantInfo> = tenants.get(&inst).cloned().unwrap_or_default();
            // a directory profile is of the identity's own organization
            let tenant_key = p.tenant_key.clone().or_else(|| {
                p.directory
                    .then(|| own_keys.first().map(|t| t.key.clone()))
                    .flatten()
            });
            let org_id = match tenant_key {
                Some(k) => {
                    let own = own_keys
                        .iter()
                        .find(|t| t.key == k && t.name.is_some())
                        .or_else(|| own_keys.iter().find(|t| t.key == k));
                    let hint = TenantHint {
                        key: k.clone(),
                        own: own.is_some(),
                        name: own.and_then(|t| t.name.clone()),
                        seen_in: seen_in.clone(),
                    };
                    let actor = cand.actor.clone();
                    let ws2 = ws.to_string();
                    match store.write(|tx| accounts::tenant_org(tx, &ws2, &actor, &hint)) {
                        Ok(o) => Some(o),
                        Err(e) => {
                            tracing::warn!(ws, "names: organization not recorded: {e}");
                            None
                        }
                    }
                }
                None => None,
            };
            let mut handles: Vec<ExtraHandle> = Vec::new();
            if let Some(h) = &p.handle {
                for x in std::iter::once(&h.handle).chain(h.extra.iter()) {
                    // `union_id`s hold everywhere; `open_id`s only under one app
                    let conn = x
                        .starts_with("ou_")
                        .then(|| inst.clone())
                        .filter(|c| !c.is_empty());
                    handles.push(ExtraHandle {
                        handle: x.clone(),
                        connection_id: conn,
                    });
                }
            }
            let e = Enrichment {
                name: Some(p.name.clone()).filter(|n| !n.trim().is_empty()),
                handles,
                email: p.email.clone(),
                avatar_url: p.avatar_url.clone(),
                org_id,
                dept: None,
                title: p.title.clone(),
                directory: p.directory,
            };
            if let Some(r) = channels.apply(ws, cand, &e, rep)
                && (r.account_named || r.renamed)
            {
                rep.named_remote += 1;
            }
        }
    }

    fn note(&self, answers: &mut Answers) {
        let (channels, ws, now, open) = (self.channels, self.ws, self.now, self.open);
        let tenants = self.tenants;
        // 5. what each of them still misses, and why — from this run's own calls about them only
        for cand in open {
            let calls = answers.asked.remove(&cand.account_id).unwrap_or_default();
            if calls.is_empty() {
                continue; // nothing asked about them this run (cached): their record stands
            }
            let hit = match_at(just_profiles(&answers.profiles), &cand.handle);
            let answered_by = hit.map(|(at, _)| answers.profiles[at].2.clone());
            let hit = hit.map(|(_, p)| p);
            let ev = Evidence {
                calls,
                own_keys: match &answered_by {
                    Some(i) => tenants
                        .get(i)
                        .into_iter()
                        .flatten()
                        .map(|t| t.key.clone())
                        .collect(),
                    None => self.own_keys.clone(),
                },
            };
            if let Err(e) = channels.note_lookup(ws, cand, hit, &ev, now) {
                tracing::warn!(
                    ws,
                    account = cand.account_id,
                    "names: lookup not noted: {e}"
                );
            }
        }
    }
}

fn named_by(
    profiles: &[(Profile, Option<String>, String)],
    acc: &str,
    cands: &[Candidate],
) -> bool {
    cands.iter().find(|c| c.account_id == acc).is_some_and(|c| {
        match_at(just_profiles(profiles), &c.handle).is_some_and(|(_, p)| !p.name.is_empty())
    })
}
