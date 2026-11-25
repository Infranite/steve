/* Devices · Hub (#/devices/hub, formerly 设置 · Hub 管理): the Hub's shared name and purpose, its version, address
 * and ID, the alias this client shows it by; the computer it runs on and its own Node; how remote Nodes reach it; and
 * how clients sign in. The shared name is written with its revision: a conflict, a failed or timed-out save is read
 * back and compared, never taken for success, and a draft is kept until it is saved or discarded. */
import "./devices.css"
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react"
import { KeyRoundIcon, MonitorIcon, NetworkIcon, ServerIcon, SlidersHorizontalIcon, SquareTerminalIcon } from "lucide-react"
import { useT } from "@/i18n"
import { date, num } from "@/i18n/format"
import { Button } from "@/components/ui/button"
import { Input, TextField } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Switch } from "@/components/ui/switch"
import { Spinner } from "@/components/ui/spinner"
import { Badge } from "@/components/ui/badge"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { FormField } from "@/components/affect/form-field-clean"
import { go } from "@/ui"
import { Select } from "@/view-shared"
import { deviceWorkEvidence, st, toast, useStore } from "@/store"
import { api, HubError, type RTailcat, type RUpgradePolicy } from "@/api"
import { access, type Oidc } from "@/api/access"
import { currentHub, setHubNavigationGuard } from "@/hub"
import { HubChooser, useHubs, useHubAddress } from "@/hub-ui"
import { hubDraftErrors, parseHubSnapshot, normalizeHubDraft, readHubInfo, writeHubInfo, type HubDraft, type HubInfo, type HubMetadata, type HubSnapshot } from "@/api/hub-management"
import { agoText, disks, hostLine, hostOf, loadPct, memPct, one, routeLabel, LOCAL_NODE } from "@/device-model"
import { useDevices, useNodeVitals } from "@/devices-data"
import { deviceGroups } from "@/devices-work"
import { Chip, DCard, DRow, Figure } from "@/devices-ui"
import { ContextDefaultRow } from "@/devices-context"

const draftOf = (m: HubMetadata): HubDraft => ({ display_name: m.display_name, purpose_note: m.purpose_note })
const equal = (a: HubDraft, b: HubDraft) => JSON.stringify(normalizeHubDraft(a)) === JSON.stringify(normalizeHubDraft(b))

/** Read-only recovery view when the shell can load but the Hub cannot bootstrap. */
export function HubOfflinePage() {
  const t = useT()
  return <main className="h-dvh overflow-y-auto bg-background p-4 text-foreground sm:p-8"><div className="mx-auto flex max-w-3xl flex-col gap-6"><h1 className="type-title">{t("devices:tab.hub")}</h1><HubCard readOnly /><Button onClick={() => location.reload()} data-hub-reconnect>{t("common:action.retry")}</Button><HubChooser down /></div></main>
}

export function HubTab() {
  const { access: v } = useDevices()
  return (
    <div className="grid items-start gap-4 lg:grid-cols-[minmax(0,1.25fr)_minmax(0,1fr)]" data-devices-hub>
      <div className="flex min-w-0 flex-col gap-4"><HubCard route={v?.route ?? null} /><HubMachine /></div>
      <div className="flex min-w-0 flex-col gap-4"><RemoteNodes /><NodeDefaults /><SignIn /></div>
    </div>
  )
}

/** a fact of the Hub card: label on the left (above it on a phone), the value */
const Fact = ({ label, children }: { label: ReactNode; children: ReactNode }) => <>
  <dt className="type-callout text-muted-foreground">{label}</dt>
  <dd className="min-w-0 type-callout text-foreground [overflow-wrap:anywhere] max-sm:mb-2">{children}</dd>
</>

/** the Hub's shared name and purpose (editable by the instance owner), version, address, ID and this client's alias */
export function HubCard({ readOnly = false, route = null }: { readOnly?: boolean; route?: string | null }) {
  const t = useT(), { dir, hubs, reload } = useHubs(), address = useHubAddress()
  useStore()
  const current = hubs?.find(h => h.current)
  const key = `steve.hub.snapshot:${dir.native ? current?.id ?? "pending" : currentHub()}`
  const [snapshot, setSnapshot] = useState<HubSnapshot | null>(() => {
    try { return parseHubSnapshot(JSON.parse(sessionStorage.getItem(key) ?? "null")) } catch { return null }
  })
  useEffect(() => {
    try { setSnapshot(parseHubSnapshot(JSON.parse(sessionStorage.getItem(key) ?? "null"))) } catch { setSnapshot(null) }
  }, [key])
  const [info, setInfo] = useState<HubInfo | null>(null)
  const [status, setStatus] = useState<"loading" | "online" | "offline" | "error">("loading")
  const [editing, setEditing] = useState(false)
  const [base, setBase] = useState<HubMetadata | null>(null)
  const [draft, setDraft] = useState<HubDraft>({ display_name: "", purpose_note: "" })
  const [previous, setPrevious] = useState<HubDraft | null>(null)
  const [conflict, setConflict] = useState<HubInfo | null>(null)
  const [uncertain, setUncertain] = useState(false)
  const [busy, setBusy] = useState(false)
  const inFlight = useRef(false)
  const readEpoch = useRef(0)
  const [message, setMessage] = useState("")
  const [validated, setValidated] = useState(false)
  const [aliasEditing, setAliasEditing] = useState(false)
  const [alias, setAlias] = useState("")
  const [aliasBase, setAliasBase] = useState("")
  const dirty = (editing && base && !equal(draft, draftOf(base))) || (aliasEditing && alias !== aliasBase)
  const leaveApproved = useRef(false)
  const guard = useRef<() => boolean>(() => true)
  guard.current = () => {
    if (inFlight.current) return false
    const allowed = !dirty || window.confirm(t("hub:discard"))
    if (allowed) leaveApproved.current = true
    return allowed
  }
  useEffect(() => { leaveApproved.current = false }, [draft, alias])
  useEffect(() => {
    setHubNavigationGuard(() => guard.current())
    const unload = (e: BeforeUnloadEvent) => { if (!leaveApproved.current && (dirty || inFlight.current)) { e.preventDefault(); e.returnValue = "" } }
    const escape = (e: KeyboardEvent) => { if (e.key === "Escape" && (editing || aliasEditing) && guard.current()) { setEditing(false); setAliasEditing(false) } }
    window.addEventListener("beforeunload", unload); window.addEventListener("keydown", escape)
    return () => { setHubNavigationGuard(null); window.removeEventListener("beforeunload", unload); window.removeEventListener("keydown", escape) }
  }, [dirty, editing, aliasEditing])
  const accept = useCallback((value: HubInfo) => {
    setInfo(value); setStatus("online")
    const snap = { metadata: value.metadata, seen: Date.now() }; setSnapshot(snap)
    try { sessionStorage.setItem(key, JSON.stringify(snap)) } catch { /* snapshot optional; no write is queued */ }
  }, [key])
  const refresh = useCallback(async () => {
    const epoch = ++readEpoch.current
    try { const value = await readHubInfo(); if (epoch === readEpoch.current) accept(value); return value }
    catch (e) { if (epoch === readEpoch.current) { setInfo(null); setStatus(e instanceof TypeError || e instanceof DOMException ? "offline" : "error") }; return null }
  }, [accept])
  useEffect(() => {
    void refresh()
    const timer = window.setInterval(() => { if (!inFlight.current) void refresh() }, 15000)
    const focus = () => { if (!inFlight.current) void refresh() }
    window.addEventListener("online", focus); window.addEventListener("focus", focus)
    return () => { clearInterval(timer); window.removeEventListener("online", focus); window.removeEventListener("focus", focus) }
  }, [refresh])
  const latest = info?.metadata ?? snapshot?.metadata
  const editable = !readOnly && status === "online" && info?.can_edit === true && !st.down
  const errors = hubDraftErrors(draft)
  const changedRemotely = editing && base && info && (base.hub_id !== info.metadata.hub_id || base.revision !== info.metadata.revision)
  const comparison = conflict ?? (changedRemotely ? info : null)
  const loadLatest = () => {
    if (!info) return
    setPrevious(draft); setDraft(draftOf(info.metadata)); setBase(info.metadata); setConflict(null); setUncertain(false); setValidated(false); setMessage("")
  }
  const save = async () => {
    if (inFlight.current || !base || !editable || comparison || uncertain) return
    setValidated(true)
    if (errors.name || errors.note) { document.getElementById(errors.name ? "hub-name" : "hub-note")?.focus(); return }
    inFlight.current = true; ++readEpoch.current; setBusy(true); setMessage("")
    try {
      const value = await writeHubInfo(base.revision, draft)
      if (value.metadata.hub_id !== base.hub_id || value.metadata.revision <= base.revision || !equal(draftOf(value.metadata), draft)) throw new Error("unconfirmed")
      accept(value); setEditing(false); setPrevious(null); setMessage(t("hub:saved"))
    } catch (e) {
      // A failed/timeout response may follow a committed write. Read back before another attempt.
      const value = await refresh()
      if (e instanceof HubError && e.status === 403) setMessage(t("hub:revoked"))
      else if (e instanceof HubError && e.status === 409) { setConflict(value); setUncertain(!value); setMessage(t("hub:conflict")) }
      else if (value && value.metadata.hub_id === base.hub_id && value.metadata.revision === base.revision + 1 && equal(draftOf(value.metadata), draft)) {
        setUncertain(true); setMessage(t("hub:readbackMatch"))
      } else { setUncertain(true); setMessage(t("hub:unknownResult")); if (value && value.metadata.revision !== base.revision) setConflict(value) }
    } finally { inFlight.current = false; setBusy(false) }
  }
  const saveAlias = async () => {
    if (!current || inFlight.current) return
    inFlight.current = true; setBusy(true); setMessage("")
    try { await dir.rename(current, alias); await reload(); setAliasEditing(false); setMessage(t("hub:aliasSaved")) }
    catch { setMessage(t("hub:aliasFailed")) }
    finally { inFlight.current = false; setBusy(false) }
  }
  const startEdit = () => { if (info) { setDraft(draftOf(info.metadata)); setBase(info.metadata); setConflict(null); setUncertain(false); setValidated(false); setEditing(true); setMessage("") } }
  const version = st.version || current?.version
  const offline = st.down ? "offline" : status
  return (
    <DCard data-hub-management icon={<NetworkIcon />} title={latest?.display_name ?? t("hub:unread")} sub={t("devices:hub.sharedSub")}
      action={!editing && <Button size="sm" variant="outline" disabled={!editable || busy || aliasEditing} onClick={startEdit} aria-label={t("hub:edit")} data-hub-edit>{t("devices:hub.edit")}</Button>}>
      <div className="flex flex-col gap-4 px-5 pb-5">
        {offline !== "online" && <div className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded-lg bg-muted/60 px-3 py-2 type-footnote text-muted-foreground" role="status" data-hub-status={offline}>
          <span className="min-w-0 flex-1"><span>{t(`hub:state.${offline}`)}</span>{snapshot && offline !== "loading" && <span className="text-tertiary"> · {t("hub:synced", { time: date(snapshot.seen, { dateStyle: "medium", timeStyle: "medium" })})}</span>}</span>
          {offline !== "loading" && <Button size="xs" variant="ghost" disabled={busy} onClick={() => void refresh()}>{t("common:action.retry")}</Button>}
        </div>}
        {message && <p role="alert" data-hub-message className="rounded-lg bg-muted/60 px-3 py-2 type-callout whitespace-pre-wrap">{message}</p>}
        {editing ? <div className="flex flex-col gap-4" data-hub-editor>
          {!editable && <p role="alert" className="type-callout text-attention">{t(status === "online" ? "hub:revoked" : "hub:offlineDraft")}</p>}
          <FormField label={t("hub:name")} error={validated && errors.name ? t("hub:nameError") : undefined} hint={t("hub:nameHint")} counter={`${[...draft.display_name.trim()].length} / 64`}>
            <Input id="hub-name" autoFocus value={draft.display_name} disabled={busy} onChange={e => setDraft({ ...draft, display_name: e.target.value })} />
          </FormField>
          <FormField label={t("hub:note")} error={validated && errors.note ? t("hub:noteError") : undefined} hint={t("hub:noteHint")} counter={`${[...normalizeHubDraft(draft).purpose_note].length} / 500`}>
            <Textarea id="hub-note" rows={5} value={draft.purpose_note} disabled={busy} onChange={e => setDraft({ ...draft, purpose_note: e.target.value })} />
          </FormField>
          {(comparison || uncertain) && <div className="space-y-3 rounded-lg bg-attention/10 p-3 type-callout" data-hub-conflict>
            <p>{t("hub:conflict")}</p>
            {info && <><strong>{t("hub:latest")}</strong><div className="whitespace-pre-wrap">{info.metadata.display_name}{"\n"}{info.metadata.purpose_note}</div></>}
            <strong>{t("hub:yourDraft")}</strong><div className="whitespace-pre-wrap">{draft.display_name}{"\n"}{draft.purpose_note}</div>
            <Button size="sm" variant="outline" disabled={busy || !info || !editable} onClick={loadLatest} data-hub-reconcile>{t("hub:loadLatest")}</Button>
          </div>}
          {previous && <div className="whitespace-pre-wrap rounded-lg bg-muted/60 p-3 type-callout" data-hub-previous><strong>{t("hub:previous")}</strong>{"\n"}{previous.display_name}{"\n"}{previous.purpose_note}</div>}
          <div className="flex flex-wrap gap-2">
            <Button size="sm" disabled={busy || !editable || !!comparison || uncertain || !dirty} onClick={() => void save()} data-hub-save>{t(busy ? "hub:saving" : "common:action.save")}</Button>
            <Button size="sm" variant="outline" disabled={busy} onClick={() => { if (guard.current()) { setEditing(false); setPrevious(null); setMessage("") } }} data-hub-cancel>{t("common:action.cancel")}</Button>
          </div>
        </div> : <p className={latest?.purpose_note ? "whitespace-pre-wrap type-body" : "type-callout text-muted-foreground"} data-hub-purpose
          title={latest?.updated_at ? t("devices:hub.updatedBy", { time: date(latest.updated_at, { dateStyle: "medium", timeStyle: "short" }), by: latest.updated_by ?? "" }) : undefined}>{latest?.purpose_note || t("hub:noNote")}</p>}
        {!editing && !editable && status !== "loading" && <p className="type-footnote text-muted-foreground" data-hub-readonly>{t(readOnly ? "hub:recoveryReadOnly" : status === "online" && !st.down ? "hub:ownerOnly" : "hub:offlineDraft")}</p>}
        <dl className="grid gap-x-6 gap-y-2.5 sm:grid-cols-[9rem_minmax(0,1fr)]">
          <Fact label={t("devices:hub.version")}><span className="tabular-nums" data-hub-version>{version || t("hub:unread")}</span>
            {st.update && <> · <a className="text-blue hover:underline" href="#/settings/about" data-hub-update>{t("devices:hub.updateAvailable", { version: st.update.version })}</a></>}</Fact>
          <Fact label={t("devices:hub.address")}>{info?.addresses.length
            /* every address the Hub answers on, as it reports them (public first); not read yet: the one this client uses */
            ? <span className="flex flex-col gap-1.5">{info.addresses.map(a => <span key={a.url} className="inline-flex flex-wrap items-center gap-2" data-hub-addresses={a.kind}><span className="type-code">{a.url}</span><Chip>{routeLabel(a.kind)}</Chip></span>)}</span>
            : <span className="inline-flex flex-wrap items-center gap-2"><span className="type-code" data-hub-address>{address || currentHub()}</span>{route && <Chip>{routeLabel(route)}</Chip>}</span>}</Fact>
          <Fact label={t("devices:hub.id")}><span className="type-code" data-hub-id>{latest?.hub_id ?? t("hub:unread")}</span></Fact>
          <Fact label={t("devices:hub.alias")}>{aliasEditing ? <div className="flex flex-col gap-2">
            <FormField label={t("hub:alias")} hint={t("hub:aliasLimit")}><Input id="hub-alias" value={alias} disabled={busy} onChange={e => setAlias(e.target.value)} /></FormField>
            <div className="flex flex-wrap gap-2"><Button size="sm" disabled={busy || alias === aliasBase} onClick={() => void saveAlias()} data-hub-alias-save>{t("common:action.save")}</Button><Button size="sm" variant="outline" disabled={busy} onClick={() => setAlias("")} data-hub-alias-clear>{t("hub:clearAlias")}</Button><Button size="sm" variant="ghost" disabled={busy} onClick={() => { if (guard.current()) setAliasEditing(false) }}>{t("common:action.cancel")}</Button></div>
            <p className="type-footnote text-muted-foreground">{t("hub:aliasHint")}</p>
          </div> : <span className="inline-flex flex-wrap items-center gap-2"><span className={current?.alias ? "" : "text-muted-foreground"} data-hub-alias-value>{current?.alias || t("hub:noAlias")}</span>
            <Button size="xs" variant="ghost" disabled={!current || busy || editing} onClick={() => { setAlias(current?.alias ?? ""); setAliasBase(current?.alias ?? ""); setAliasEditing(true); setMessage("") }} aria-label={t("hub:editAlias")} data-hub-alias-edit>{t("devices:hub.setAlias")}</Button></span>}</Fact>
        </dl>
      </div>
    </DCard>
  )
}

/** the computer the Hub runs on: its load, memory and disk, and the way into its own Node */
function HubMachine() {
  const t = useT(), { views } = useDevices()
  useStore()
  const local = views.find(v => v.local)
  const { value: v, stale } = useNodeVitals(LOCAL_NODE)
  const l = v?.latest, lp = loadPct(l), mp = memPct(l), disk = disks(l)[0]
  const d = local?.device
  const ask = d && deviceWorkEvidence().loaded ? deviceGroups(d.id).attention.length : 0
  /* the system as its Node or the device row says it, then the cores and memory it reported */
  const facts = local ? hostLine(hostOf(local, l)) : ""
  return (
    <DCard icon={<MonitorIcon />} title={t("devices:hub.machine")} sub={facts || d?.name} data-hub-machine>
      <div className="grid grid-cols-2 gap-x-6 gap-y-4 px-5 pb-4 sm:grid-cols-3">
        {!l ? <p className="col-span-full type-callout text-muted-foreground">{t("devices:vitals.none")}</p> : <>
          <Figure label={t("devices:vitals.cpu")} value={lp != null ? `${lp}%` : "—"} fill={lp != null ? lp / 100 : null} data-vital="cpu"
            title={l.load ? t("devices:vitals.cpuTip", { values: l.load.map(x => num(one(x))).join(" / ") }) : undefined}
            sub={l.load && l.cpus ? t("devices:vitals.cpuSub", { load: num(one(l.load[0])), cpus: l.cpus }) : undefined} />
          <Figure label={t("devices:vitals.mem")} value={mp != null ? `${mp}%` : "—"} fill={mp != null ? mp / 100 : null} data-vital="memory"
            sub={l.mem_total && l.mem_available != null ? t("devices:vitals.memSub", { used: Math.round((l.mem_total - l.mem_available) / 2 ** 30), total: Math.round(l.mem_total / 2 ** 30) }) : undefined} />
          {disk && <Figure label={<>{t("devices:vitals.disk")} <span className="type-code text-tertiary">{disk.name}</span></>} value={t("devices:vitals.diskFree", { n: num(disk.free) })} fill={disk.used} data-vital="disk"
            sub={t("devices:vitals.diskTotal", { n: num(disk.total) })} />}
          {stale && <p className="col-span-full type-footnote text-attention" data-vitals-stale>{t("devices:vitals.stale", { ago: agoText(l.sampled_at ?? l.at) })}</p>}
        </>}
      </div>
      {local && <div className="border-t border-border/70"><DRow icon={<SquareTerminalIcon />} onClick={() => go("#/devices/nodes/" + LOCAL_NODE)} data-local-node
        title={<>{t("devices:map.localNode")}<Chip>{t("devices:map.builtIn")}</Chip></>}
        sub={<>{local.max != null && local.running != null && <span className="tabular-nums">{t("devices:hub.localSessions", { n: local.running, max: local.max })}</span>}
          {(local.waiting ?? 0) > 0 && <span className="text-attention">{t("devices:stat.queued", { n: local.waiting })}</span>}
          {ask > 0 && <span className="text-attention">{t("devices:hub.ask", { count: ask })}</span>}</>} /></div>}
    </DCard>
  )
}

/** how other computers reach this Hub: whether it listens for Nodes, tailcat, and the upgrade policy Nodes follow */
function RemoteNodes() {
  const t = useT(), { nodesRead, nodesError, nodesOff } = useDevices()
  const owner = st.owner
  const [error, setError] = useState("")
  const [tailcat, setTailcat] = useState<RTailcat | null>(null), [busy, setBusy] = useState(false)
  useEffect(() => {
    setTailcat(null); setError("")
    if (!owner || !nodesRead || nodesOff) return
    let live = true
    void api.nodesTailcat().then(
      (v) => { if (live) setTailcat(v) },
      (e: Error) => { if (live) setError(e.message) },
    )
    return () => { live = false }
  }, [owner, nodesRead, nodesOff])
  if (!owner) return null
  const toggle = async (on: boolean) => {
    setBusy(true)
    try { setTailcat(await api.setNodesTailcat(on)); setError("") }
    catch (e) {
      setError((e as Error).message)
      void api.nodesTailcat().then(setTailcat, () => {})
    }
    finally { setBusy(false) }
  }
  return (
    <DCard icon={<ServerIcon />} title={t("devices:hub.remote")} sub={t("devices:hub.remoteSub")} data-hub-remote>
      <div className="border-t border-border/70">
        {(nodesError || error) && <p role="alert" className="px-5 py-3 type-callout text-destructive">{nodesError || error}</p>}
        {nodesRead && <DRow title={t("devices:hub.accept")} sub={nodesOff ? t("devices:hub.acceptOffSub") : t("devices:hub.acceptSub")} data-nodes-off={nodesOff ? "" : undefined}>
          <Badge tone={nodesOff ? "neutral" : "success"} size="sm" dot title={nodesOff ? t("devices:hub.acceptOffTip") : undefined}>{t(nodesOff ? "devices:hub.off" : "devices:hub.on")}</Badge>
        </DRow>}
        {nodesRead && !nodesOff && tailcat && <DRow title={t("devices:hub.tailcat")} data-tailcat={tailcat.enabled ? "on" : "off"}
          sub={tailcat.available || tailcat.enabled ? <>{t("devices:hub.tailcatSub")}{tailcat.enabled && tailcat.address && <span className="type-code">{tailcat.address}</span>}{tailcat.enabled && !tailcat.running && tailcat.error && <span className="text-destructive">{tailcat.error}</span>}</> : t("devices:hub.tailcatMissing")}>
          {(tailcat.available || tailcat.enabled) && <>{busy && <Spinner className="size-3.5 text-muted-foreground" />}<Switch checked={tailcat.enabled} disabled={busy} onCheckedChange={(on) => void toggle(on)} aria-label={t("devices:hub.tailcat")} data-tailcat-switch /></>}
        </DRow>}
      </div>
    </DCard>
  )
}

/** what every Node follows unless it chooses for itself: the upgrade policy and the personal agent config (the
 *  instance owner sets them; anyone else sees them) */
function NodeDefaults() {
  const t = useT(), { upgradeDefault, contextDefault, reloadDefaults } = useDevices()
  useStore()
  const [policy, setPolicy] = useState<RUpgradePolicy | null>(upgradeDefault?.policy ?? null)
  useEffect(() => setPolicy(upgradeDefault?.policy ?? null), [upgradeDefault?.policy])
  if (!policy && !contextDefault) return null
  const setDefault = async (v: string) => {
    const prev = policy
    setPolicy(v as RUpgradePolicy)
    try { setPolicy((await api.setNodeUpgradeDefault(v as RUpgradePolicy)).policy); reloadDefaults() } catch (e) { setPolicy(prev); toast((e as Error).message) }
  }
  return (
    <DCard icon={<SlidersHorizontalIcon />} title={t("devices:hub.defaults")} sub={t("devices:hub.defaultsSub")} data-hub-defaults>
      <div className="border-t border-border/70">
        {policy && <DRow title={t("devices:hub.upgradeDefault")} sub={t("devices:hub.upgradeDefaultSub")} className="flex-wrap">
          <Select align="end" testId="node-upgrade-default" label={t("devices:hub.upgradeDefault")} value={policy} onChange={(v) => void setDefault(v)} disabled={!st.owner}
            options={(["auto", "notify", "off"] as const).map(v => ({ v, l: t(`device:upgrade.policy.${v}`) }))} />
        </DRow>}
        <ContextDefaultRow editable={st.owner} />
      </div>
    </DCard>
  )
}

/** how clients sign in: a user name and an access token always; single sign-on when the owner set it up */
function SignIn() {
  const t = useT()
  const [o, setO] = useState<Oidc | null>(null), [open, setOpen] = useState(false)
  useEffect(() => { let live = true; access.oidc().then(x => { if (live) setO(x) }, () => {}); return () => { live = false } }, [])
  return (
    <DCard icon={<KeyRoundIcon />} title={t("devices:hub.signIn")} sub={t("devices:hub.signInSub")} data-hub-signin>
      <div className="border-t border-border/70">
        <DRow title={t("devices:hub.tokens")} sub={t("devices:hub.tokensSub")}><Button size="sm" variant="ghost" onClick={() => go("#/devices/clients")} data-hub-tokens>{t("devices:hub.manage")}</Button></DRow>
        {/* single sign-on is the owner's: the Hub refuses anyone else, and then the row is not shown */}
        {o && <DRow title={t("devices:hub.oidc")} data-oidc={o.source}
          sub={o.source === "none" ? t("devices:hub.oidcNone") : t(o.source === "env" ? "devices:hub.oidcEnv" : "devices:hub.oidcOn", { issuer: o.issuer ?? "" })}>
          {o.source !== "env" && <Button size="sm" variant="outline" onClick={() => setOpen(true)} data-oidc-open>{t(o.source === "none" ? "devices:hub.oidcConfigure" : "devices:hub.manage")}</Button>}
        </DRow>}
      </div>
      {open && o && <OidcDialog o={o} onSaved={setO} onClose={() => setOpen(false)} />}
    </DCard>
  )
}

/** single sign-on (owner only): an issuer, a client ID and a secret; set from here, or turned off */
function OidcDialog({ o, onSaved, onClose }: { o: Oidc; onSaved: (o: Oidc) => void; onClose: () => void }) {
  const t = useT()
  const [f, setF] = useState({ issuer: o.issuer ?? "", client_id: o.client_id ?? "", secret: "" })
  const [busy, setBusy] = useState(false)
  const save = async () => {
    setBusy(true)
    try { const x = await access.saveOidc({ issuer: f.issuer.trim(), client_id: f.client_id.trim(), ...(f.secret.trim() ? { secret: f.secret.trim() } : {}) }); onSaved(x); toast(t("pages:access.oidcSaved")); onClose() }
    catch (e) { toast((e as Error).message) }
    setBusy(false)
  }
  const off = async () => { try { onSaved(await access.dropOidc()); onClose() } catch (e) { toast((e as Error).message) } }
  const field = (label: string, input: ReactNode) => <label className="grid gap-1.5"><span className="type-callout text-muted-foreground">{label}</span>{input}</label>
  return (
    <Dialog open onOpenChange={(x) => !x && onClose()}>
      <DialogContent className="sm:max-w-md" data-oidc-dialog>
        <DialogHeader><DialogTitle>{t("devices:hub.oidc")}</DialogTitle><DialogDescription>{t("pages:access.oidcSub")}</DialogDescription></DialogHeader>
        <div className="flex flex-col gap-3">
          {field(t("pages:access.oidcCallback"), <span className="type-code break-all">{o.callback}</span>)}
          {field("Issuer", <TextField size="sm" value={f.issuer} onChange={(e) => setF({ ...f, issuer: e.target.value })} placeholder="https://id.example.com/application/o/steve/" data-oidc-issuer />)}
          {field(t("pages:access.oidcClient"), <TextField size="sm" value={f.client_id} onChange={(e) => setF({ ...f, client_id: e.target.value })} placeholder={t("pages:access.oidcClient")} data-oidc-client />)}
          {field(t("pages:access.oidcSecret"), <TextField size="sm" type="password" value={f.secret} onChange={(e) => setF({ ...f, secret: e.target.value })} placeholder={o.has_secret ? t("pages:access.oidcSecretKept") : t("pages:access.oidcSecret")} autoComplete="new-password" data-oidc-secret />)}
        </div>
        <DialogFooter>
          {o.source === "settings" && <Button variant="ghost" className="mr-auto text-muted-foreground" onClick={() => void off()} data-oidc-off>{t("pages:access.oidcOff")}</Button>}
          <Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button>
          <Button onClick={() => void save()} disabled={busy || !f.issuer.trim() || !f.client_id.trim()} data-oidc-save>{busy ? <Spinner className="size-4" /> : t("pages:access.oidcSave")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
