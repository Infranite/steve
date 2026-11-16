import "./hub-settings.css"
import { useCallback, useEffect, useRef, useState } from "react"
import { ServerIcon } from "lucide-react"
import { useT } from "@/i18n"
import { date } from "@/i18n/format"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { FormField } from "@/components/affect/form-field-clean"
import { IntegrationSettings, IntegrationSection } from "@/components/affect/integration-settings-clean"
import { Section, Panel, Row, openSheet } from "@/ui"
import { st } from "@/store"
import { HubError } from "@/api"
import { currentHub, setHubNavigationGuard } from "@/hub"
import { HubChooser, useHubs, useHubAddress } from "@/hub-ui"
import { hubDraftErrors, linkedHubDevice, parseHubSnapshot, normalizeHubDraft, readHubInfo, writeHubInfo, UnsupportedHubMetadata, type HubDraft, type HubInfo, type HubMetadata, type HubSnapshot } from "@/hub-management-api"

const draftOf = (m: HubMetadata): HubDraft => ({ display_name: m.display_name, purpose_note: m.purpose_note })
const equal = (a: HubDraft, b: HubDraft) => JSON.stringify(normalizeHubDraft(a)) === JSON.stringify(normalizeHubDraft(b))

/** Read-only recovery view when the shell can load but the Hub cannot bootstrap. */
export function HubOfflinePage() {
  const t = useT()
  return <main className="h-dvh overflow-y-auto bg-background p-4 text-foreground sm:p-8"><div className="mx-auto flex max-w-3xl flex-col gap-6"><h1 className="type-title">{t("hub:title")}</h1><HubSettingsPane readOnly /><Button onClick={() => location.reload()} data-hub-reconnect>{t("common:action.retry")}</Button><HubChooser down /></div></main>
}

export function HubSettingsPane({ readOnly = false }: { readOnly?: boolean }) {
  const t = useT(), { dir, hubs, reload } = useHubs(), address = useHubAddress()
  const current = hubs?.find(h => h.current)
  const device = linkedHubDevice(st.devices)
  const key = `steve.hub.snapshot:${dir.native ? current?.id ?? "pending" : currentHub()}`
  const [snapshot, setSnapshot] = useState<HubSnapshot | null>(() => {
    try { return parseHubSnapshot(JSON.parse(sessionStorage.getItem(key) ?? "null")) } catch { return null }
  })
  useEffect(() => {
    try { setSnapshot(parseHubSnapshot(JSON.parse(sessionStorage.getItem(key) ?? "null"))) } catch { setSnapshot(null) }
  }, [key])
  const [info, setInfo] = useState<HubInfo | null>(null)
  const [status, setStatus] = useState<"loading" | "online" | "offline" | "unsupported">("loading")
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
    catch (e) { if (epoch === readEpoch.current) { setInfo(null); setStatus(e instanceof UnsupportedHubMetadata ? "unsupported" : "offline") }; return null }
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
    if (!dir.rename || !current || inFlight.current) return
    inFlight.current = true; setBusy(true); setMessage("")
    try { await dir.rename(current, alias); await reload(); setAliasEditing(false); setMessage(t("hub:aliasSaved")) }
    catch { setMessage(t("hub:aliasFailed")) }
    finally { inFlight.current = false; setBusy(false) }
  }
  return <div className="flex min-w-0 flex-col gap-7 [overflow-wrap:anywhere]" data-hub-management>
    <div className="flex flex-wrap items-center gap-2">
      <span role="status" data-hub-status>{t(`hub:state.${st.down ? "offline" : status}`)}</span>
      {snapshot && <span className="type-footnote text-muted-foreground">{t("hub:synced", { time: date(snapshot.seen, { dateStyle: "medium", timeStyle: "medium" }) })}</span>}
      <Button variant="ghost" size="sm" disabled={busy} onClick={() => void refresh()}>{t("common:action.retry")}</Button>
    </div>
    {message && <p role="alert" data-hub-message className="rounded-lg border p-3 whitespace-pre-wrap">{message}</p>}
    <IntegrationSettings name={latest?.display_name ?? t("hub:unread")} logo={<ServerIcon />} meta={t("hub:shared")} className="[--ui-isc-width:100%]">
      <IntegrationSection>
        {editing ? <div className="flex flex-col gap-4" data-hub-editor>
          {!editable && <p role="alert">{t(status === "online" ? "hub:revoked" : "hub:offlineDraft")}</p>}
          <FormField label={t("hub:name")} error={validated && errors.name ? t("hub:nameError") : undefined} hint={t("hub:nameHint")} counter={`${[...draft.display_name.trim()].length} / 64`}>
            <Input id="hub-name" autoFocus value={draft.display_name} disabled={busy} onChange={e => setDraft({ ...draft, display_name: e.target.value })} />
          </FormField>
          <FormField label={t("hub:note")} error={validated && errors.note ? t("hub:noteError") : undefined} hint={t("hub:noteHint")} counter={`${[...normalizeHubDraft(draft).purpose_note].length} / 500`}>
            <Textarea id="hub-note" rows={5} value={draft.purpose_note} disabled={busy} onChange={e => setDraft({ ...draft, purpose_note: e.target.value })} />
          </FormField>
          {(comparison || uncertain) && <div className="space-y-3 rounded-lg border p-3" data-hub-conflict>
            <p>{t("hub:conflict")}</p>
            {info && <><strong>{t("hub:latest")}</strong><div className="whitespace-pre-wrap">{info.metadata.display_name}{"\n"}{info.metadata.purpose_note}</div></>}
            <strong>{t("hub:yourDraft")}</strong><div className="whitespace-pre-wrap">{draft.display_name}{"\n"}{draft.purpose_note}</div>
            <Button variant="outline" disabled={busy || !info || !editable} onClick={loadLatest} data-hub-reconcile>{t("hub:loadLatest")}</Button>
          </div>}
          {previous && <div className="whitespace-pre-wrap rounded-lg border p-3" data-hub-previous><strong>{t("hub:previous")}</strong>{"\n"}{previous.display_name}{"\n"}{previous.purpose_note}</div>}
          <div className="flex flex-wrap gap-2">
            <Button disabled={busy || !editable || !!comparison || uncertain || !dirty} onClick={() => void save()} data-hub-save>{t(busy ? "hub:saving" : "common:action.save")}</Button>
            <Button variant="outline" disabled={busy} onClick={() => { if (guard.current()) { setEditing(false); setPrevious(null); setMessage("") } }} data-hub-cancel>{t("common:action.cancel")}</Button>
          </div>
        </div> : <div className="space-y-3">
          <p className="whitespace-pre-wrap" data-hub-purpose>{latest?.purpose_note || t("hub:noNote")}</p>
          {latest?.updated_at && <p className="type-footnote text-muted-foreground">{t("hub:updated", { time: date(latest.updated_at, { dateStyle: "medium", timeStyle: "medium" }), by: latest.updated_by ?? "" })}</p>}
          <Button disabled={!editable || busy || aliasEditing} onClick={() => { if (info) { setDraft(draftOf(info.metadata)); setBase(info.metadata); setConflict(null); setUncertain(false); setValidated(false); setEditing(true); setMessage("") } }} data-hub-edit>{t("hub:edit")}</Button>
          {!editable && <p className="type-footnote text-muted-foreground">{t(readOnly ? "hub:recoveryReadOnly" : status === "unsupported" ? "hub:unsupported" : status === "online" && !st.down ? "hub:ownerOnly" : "hub:offlineDraft")}</p>}
        </div>}
      </IntegrationSection>
    </IntegrationSettings>
    <Section label={t("hub:identity")}><Panel><div className="space-y-3 p-4">
      <dl className="space-y-3"><div><dt className="text-muted-foreground">{t("hub:address")}</dt><dd className="font-mono">{address || currentHub()}</dd></div><div><dt className="text-muted-foreground">{t("hub:id")}</dt><dd className="font-mono" data-hub-id>{latest?.hub_id ?? t("hub:unread")}</dd></div><div><dt className="text-muted-foreground">{t("hub:version")}</dt><dd>{st.version || current?.version || t("hub:unread")}</dd></div></dl>
      <p className="type-footnote text-muted-foreground">{t("hub:identityHint")}</p>
    </div></Panel></Section>
    <Section label={t("hub:devices")}><Panel><div data-hub-device={device?.id ?? ""}><Row title={device?.name ?? t("hub:deviceLink")} sub={t(device ? "hub:linkedDevice" : "hub:unlinked")} onClick={() => { location.hash = device ? `#/devices/${encodeURIComponent(device.id)}` : "#/devices" }} /></div></Panel></Section>
    <Section label={t("hub:alias")}><Panel><div className="space-y-3 p-4">
      <p className="type-callout text-muted-foreground">{t("hub:aliasHint")}</p>
      {aliasEditing ? <><FormField label={t("hub:alias")} hint={t("hub:aliasLimit")}><Input id="hub-alias" value={alias} disabled={busy} onChange={e => setAlias(e.target.value)} /></FormField><div className="flex flex-wrap gap-2"><Button disabled={busy || alias === aliasBase} onClick={() => void saveAlias()} data-hub-alias-save>{t("common:action.save")}</Button><Button variant="outline" disabled={busy} onClick={() => setAlias("")} data-hub-alias-clear>{t("hub:clearAlias")}</Button><Button variant="ghost" disabled={busy} onClick={() => { if (guard.current()) setAliasEditing(false) }}>{t("common:action.cancel")}</Button></div></> : <><p data-hub-alias-value>{current?.alias || t(dir.rename ? "hub:noAlias" : "hub:oldBridge")}</p><Button variant="outline" disabled={!dir.rename || !current || busy || editing} onClick={() => { setAlias(current?.alias ?? ""); setAliasBase(current?.alias ?? ""); setAliasEditing(true); setMessage("") }} data-hub-alias-edit>{t("hub:editAlias")}</Button></>}
    </div></Panel></Section>
    <div className="flex flex-wrap gap-3"><a href="#/settings/access">{t("hub:access")}</a>{!readOnly && <Button variant="ghost" disabled={busy} onClick={() => openSheet("hubs")}>{t("hub:switch")}</Button>}</div>
  </div>
}
