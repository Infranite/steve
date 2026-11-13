/* 飞书 / Lark (feishu-lark-cli.md §2.1/§2.4/§4, feishu.md §2.5; prototype channels.tsx AddChannelAccDialog): one way in on
 * the 接入 page. The step-by-step guide for the developer console, the bot's App ID / Secret with the permission check
 * from `verify`, and “授权你本人” through the same app (OAuth or device code). Only this file knows Feishu's flow. */
import { useEffect, useRef, useState } from "react"
import { CheckIcon, CircleHelpIcon, ExternalLinkIcon, LockIcon, XIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { Checkbox } from "@/components/ui/checkbox"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { go, SectionLabel, DiscChevron, Disclosure, DISC_TRIGGER, SUMMARY } from "@/ui"
import { Select, ChoiceCards } from "@/view-shared"
import { toast } from "@/store"
import { HubError } from "@/api"
import { t, useT } from "@/i18n"
import { useBusy, report } from "@/accounts/load"
import { accountsApi, type RAdapter, type RDevice, type RFieldDef } from "@/api/accounts"
import { vaultApi } from "@/api/vault"
import { channelsApi, FEISHU_APP, type PermissionCheck, type Persona } from "@/api/channels"
import { FeishuRegistrationPanel } from "./registration"
import { officialConfirmationUrl } from "./registration-state"
import { offers, routesOf, selfSideOf, startsOf } from "./sides"
import { asAdapter, type IAdapter } from "@/api/integrations"
import { ConnectDialog } from "@/accounts/dialogs"

/* ───────── the permission table (feishu.md §2.5 with feishu-lark-cli.md §3) ─────────
 * What to enable in the developer console, shown before connecting; after `verify` the Hub's check replaces it. */
const levelLabel = (l: PermissionCheck["level"]) => t(`integrations:feishu.level.${l}`)
type PermKey = "message" | "sendAsBot" | "p2p" | "groupAt" | "groupAll" | "chat" | "resource" | "contact" | "department" | "user" | "email"
/** [scope, what it is for (resolved at render), level] */
export const BOT_PERMS: [string, PermKey, PermissionCheck["level"]][] = [
  ["im:message", "message", "required"], ["im:message:send_as_bot", "sendAsBot", "required"], ["im:message.p2p_msg:readonly", "p2p", "required"],
  ["im:message.group_at_msg:readonly", "groupAt", "required"], ["im:message.group_msg", "groupAll", "suggested"],
  ["im:chat:readonly", "chat", "required"], ["im:resource", "resource", "required"],
  ["contact:contact.base:readonly", "contact", "directory"], ["contact:department.base:readonly", "department", "directory"], ["contact:user.base:readonly", "user", "directory"], ["contact:user.email:readonly", "email", "directory"],
]
export const userPerms = () => t("integrations:feishu.userPerms")
const STATUS: Record<PermissionCheck["status"], [() => string, any, string]> = {
  granted: [() => t("integrations:feishu.status.granted"), CheckIcon, "text-success"], missing: [() => t("integrations:feishu.status.missing"), XIcon, "text-destructive"],
  unknown: [() => t("integrations:feishu.status.unknown"), CircleHelpIcon, "text-muted-foreground"], not_requested: [() => t("integrations:feishu.status.notRequested"), CircleHelpIcon, "text-muted-foreground"],
}

export function PermissionList({ checks }: { checks: PermissionCheck[] | null }) {
  const t = useT()
  const rows: PermissionCheck[] = checks ?? BOT_PERMS.map(([scope, purpose, level]) => ({ scope, purpose: t(`integrations:feishu.perm.${purpose}`), level, status: "unknown" }))
  const missing = rows.filter((r) => r.status === "missing" && r.level === "required")
  return (
    <div className="flex flex-col gap-1.5" data-permissions={checks ? "checked" : "static"}>
      {checks && missing.length > 0 && <div className="rounded-lg bg-destructive/10 px-3 py-2 type-footnote text-destructive" role="alert">{t("integrations:feishu.missingRequired", { count: missing.length })}</div>}
      <ul className="flex flex-col divide-y rounded-xl bg-muted/60" aria-label={t("integrations:feishu.permissions")}>{rows.map((r) => { const [label, I, c] = STATUS[r.status] ?? STATUS.unknown, l = label(); return (
        <li key={r.scope} className="flex items-start gap-2.5 px-3.5 py-2.5" data-perm={r.scope} data-perm-status={r.status}>
          {checks ? <I className={"mt-0.5 size-4 shrink-0 " + c} aria-label={l} /> : <span className="mt-1.5 size-1.5 shrink-0 rounded-full bg-tertiary" aria-hidden />}
          <div className="min-w-0 flex-1"><div className="type-callout">{r.purpose}</div><div className="type-footnote text-muted-foreground">{levelLabel(r.level)}{checks ? ` · ${l}` : ""}</div><details className="group/disc mt-1 type-footnote text-muted-foreground"><summary className={SUMMARY}><DiscChevron className="size-3" />{t("integrations:feishu.scopeId")}</summary><code className="mt-0.5 block break-all pl-4 font-mono">{r.scope}</code></details></div>
        </li>) })}</ul>
      {!checks && <div className="type-footnote text-muted-foreground">{t("integrations:feishu.staticNote")}</div>}
    </div>
  )
}

function Modal({ title, desc, onClose, children, footer, w = 520 }: { title: string; desc?: string; onClose: () => void; children: React.ReactNode; footer?: React.ReactNode; w?: number }) {
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-h-[90dvh] gap-4 overflow-y-auto rounded-2xl" style={{ maxWidth: w }}>
        <DialogHeader><DialogTitle className="type-headline">{title}</DialogTitle>{desc && <DialogDescription className="type-callout">{desc}</DialogDescription>}</DialogHeader>
        {children}
        {footer && <DialogFooter className="gap-2">{footer}</DialogFooter>}
      </DialogContent>
    </Dialog>
  )
}

/** the steps in the developer console, in the current language */
export const consoleSteps = () => (["s1", "s2", "s3", "s4", "s5", "s6"] as const).map((k) => t(`integrations:feishu.console.${k}`))
/** fields the Hub asks for; without the adapter list, App ID and App Secret */
const FALLBACK_FIELDS: RFieldDef[] = [{ key: "app_id", label: "App ID", secret: false, required: true, type: "text" }, { key: "app_secret", label: "App Secret", secret: true, required: true, type: "password" }]

type Done = { connection_id: string; account_id: string; permissions: PermissionCheck[] | null; bound: boolean; webhook: boolean }

/** `side`: the side acting as you through these bots (`null`: none, so only the bot is offered; absent: the built-in) */
export function FeishuConnectDialog({ bots, onClose, initial = "bot", side }: { bots: { id: string; name: string }[]; onClose: () => void; initial?: "bot" | "self"; side?: IAdapter | null }) {
  const t = useT()
  const [kind, setKind] = useState<"bot" | "self">(side === null ? "bot" : initial), [vals, setVals] = useState<Record<string, string>>({}), [persona, setPersona] = useState("")
  const [fields, setFields] = useState<RFieldDef[]>(FALLBACK_FIELDS), [personas, setPersonas] = useState<Persona[]>([]), [noVault, setNoVault] = useState(false)
  const [advanced, setAdvanced] = useState(false)
  const [app, setApp] = useState(bots[0]?.id ?? ""), [done, setDone] = useState<Done | null>(null), [busy, act] = useBusy()
  useEffect(() => {
    vaultApi.status().then((s) => setNoVault(!s.initialized), () => {})
    accountsApi.adapters().then((xs: RAdapter[]) => { const f = xs.find((x) => x.id === FEISHU_APP)?.fields; if (f?.length) setFields(f) }, () => {})
    channelsApi.personas().then((ps) => { setPersonas(ps); setPersona((p) => p || ps.find((x) => x.is_default)?.id || ps[0]?.id || "") }, () => {})
  }, [])
  const val = (f: RFieldDef) => vals[f.key] ?? f.default ?? ""
  const ok = fields.every((f) => !f.required || val(f).trim())
  const submit = async () => {
    const body: Record<string, string> = {}
    for (const f of fields) { const v = val(f).trim(); if (v) body[f.key] = v }
    const r = await act(() => channelsApi.connectBot(body, persona || undefined))
    setVals({}) // secrets are not kept once sent
    if (r) setDone({ ...r, webhook: !!(body.verification_token || body.encrypt_key) })
  }
  if (noVault) return (
    <Modal title={t("integrations:feishu.connectTitle")} desc={t("integrations:feishu.noVaultDesc")} onClose={onClose} w={440}
      footer={<><Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button><Button onClick={() => { onClose(); go("#/vault") }}>{t("integrations:feishu.vaultGo")}</Button></>}><span hidden /></Modal>
  )
  if (done) return <Connected d={done} persona={personas.find((p) => p.id === persona)} onClose={onClose} side={side} />
  return (
    <Modal title={t("integrations:feishu.connectTitle")} desc={t("integrations:feishu.connectDesc")} onClose={onClose}
      footer={<><Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button>
        {kind === "bot" ? <Button disabled={busy || !ok} onClick={() => void submit()} data-connect-go>{busy && <Spinner />}{t("integrations:feishu.verifyConnect")}</Button>
          : <SelfButtons app={app} disabled={!app} side={side} />}</>}>
      {side !== null && <ChoiceCards value={kind} onChange={setKind} options={[{ v: "bot", t: t("integrations:feishu.botTitle"), d: t("integrations:feishu.botDesc"), test: "bot" }, { v: "self", t: t("integrations:feishu.selfTitle"), d: t("integrations:feishu.selfDesc"), test: "self" }]} />}
      {kind === "bot" ? <form className="contents" autoComplete="off" onSubmit={(e) => { e.preventDefault(); if (ok && !busy) void submit() }}>
        <FieldGroup className="gap-4">
          {fields.filter((f) => advanced || !["verification_token", "encrypt_key"].includes(f.key)).map((f) => (
            <Field key={f.key}>
              <FieldLabel htmlFor={"fs-" + f.key}>{f.label}{f.secret && <LockIcon className="ml-1 inline size-3 text-muted-foreground" aria-label={t("integrations:feishu.secret")} />}{!f.required && <span className="ml-1 font-normal text-muted-foreground">{t("integrations:feishu.optionalTag")}</span>}</FieldLabel>
              {f.type === "select" && f.key === "domain" ? <div><Select testId="fs-domain" value={val(f) || "feishu"} onChange={(v) => setVals({ ...vals, domain: v })} options={[{ v: "feishu", l: t("integrations:feishu.brand") }, { v: "lark", l: "Lark" }]} /></div>
                : <Input id={"fs-" + f.key} className="rounded-lg font-mono" type={f.secret ? "password" : "text"} autoComplete="off" spellCheck={false} required={f.required} value={val(f)} onChange={(e) => setVals({ ...vals, [f.key]: e.target.value })} data-cred-field={f.key} />}
              {f.hint && f.key !== "domain" && <FieldDescription>{f.hint}</FieldDescription>}
            </Field>))}
          <Disclosure open={advanced} onOpenChange={setAdvanced} label={t("integrations:feishu.advancedWebhook")}><div className="pl-[18px] pt-2 type-footnote text-muted-foreground">{t("integrations:feishu.advancedWebhookNote")}</div></Disclosure>
          {personas.length > 0 && <Field orientation="horizontal" className="justify-between"><div><FieldLabel>{t("integrations:feishu.persona")}</FieldLabel><FieldDescription>{t("integrations:feishu.personaSub")}</FieldDescription></div>
            <Select align="end" testId="acc-persona" value={persona} onChange={setPersona} options={personas.map((p) => ({ v: p.id, l: p.name }))} /></Field>}
        </FieldGroup>
        <Collapsible><CollapsibleTrigger className={DISC_TRIGGER} data-console-steps><DiscChevron />{t("integrations:feishu.consoleSteps")}</CollapsibleTrigger>
          <CollapsibleContent><div className="pt-3 flex flex-col gap-3">
            <ol className="list-decimal pl-5 type-callout text-muted-foreground">{consoleSteps().map((s) => <li key={s}>{s}</li>)}</ol>
            <PermissionList checks={null} />
            <div className="type-footnote text-muted-foreground">{t("integrations:feishu.userPermsLine", { perms: userPerms() })}</div>
            <div className="type-footnote text-muted-foreground">{t("integrations:feishu.oneReceiver")}</div>
          </div></CollapsibleContent></Collapsible>
        <button type="submit" hidden />
      </form> : bots.length ? <FieldGroup className="gap-4">
        <Field orientation="horizontal" className="justify-between"><div><FieldLabel>{t("integrations:feishu.viaApp")}</FieldLabel><FieldDescription>{t("integrations:feishu.viaAppSub")}</FieldDescription></div>
          <Select align="end" testId="self-app" value={app} onChange={setApp} options={bots.map((a) => ({ v: a.id, l: a.name }))} /></Field>
        <div className="type-footnote text-muted-foreground">{t("integrations:feishu.selfNote")}</div><details className="group/disc type-footnote text-muted-foreground"><summary className={SUMMARY}><DiscChevron className="size-3" />{t("integrations:feishu.showScopes")}</summary><div className="mt-0.5 pl-4">{userPerms()}</div></details>
      </FieldGroup> : <div className="rounded-xl bg-muted/60 px-4 py-3.5 type-callout text-muted-foreground">{t("integrations:feishu.botFirst")}</div>}
    </Modal>
  )
}

function Connected({ d, persona, onClose, side }: { d: Done; persona?: Persona; onClose: () => void; side?: IAdapter | null }) {
  const t = useT()
  return (
    <Modal title={t("integrations:feishu.connectedTitle")} desc={t("integrations:feishu.connectedDesc")} onClose={onClose}
      footer={<>{side !== null && <SelfButtons app={d.connection_id} variant="outline" label={t("integrations:feishu.authorizeSelfOptional")} side={side} />}<Button onClick={() => { onClose(); go("#/channels/acc/" + d.connection_id) }} data-goto-acc>{t("integrations:feishu.goSettings")}</Button></>}>
      <div className="flex flex-col gap-4" data-connected={d.connection_id}>
        <section><SectionLabel>{t("integrations:feishu.permCheck")}</SectionLabel>
          {d.permissions ? <PermissionList checks={d.permissions} /> : <div className="rounded-xl bg-muted/60 px-4 py-3 type-callout text-muted-foreground" data-permissions="none">{t("integrations:feishu.noPermCheck")}</div>}</section>
        <div className="type-callout text-muted-foreground">{t("integrations:feishu.longConn")}{d.webhook && <> {t("integrations:feishu.webhookNote")}</>}</div>
        {!d.bound && <div className="type-footnote text-muted-foreground">{persona && !persona.is_default ? t("integrations:feishu.notBoundPersona", { name: persona.name }) : t("integrations:feishu.notBound")}</div>}
      </div>
    </Modal>
  )
}

/** Device authorization works without a public URL. Redirect OAuth is advanced and opt-in. */
const USER_READ_SCOPES = ["offline_access", "im:message", "im:message:readonly", "im:message.p2p_msg:get_as_user", "im:message.group_msg:get_as_user", "im:chat:read"]

/** “授权你本人” through bot connection `app`, the ways `side` declares (absent: the built-in's, the device code first and
 * OAuth when `oauthAvailable`): a device code, OAuth, its credential form, or a page of its own */
export function SelfButtons({ app, disabled, variant = "default", label, scopes, reauthOf, oauthAvailable = false, side }: { app: string; disabled?: boolean; variant?: "default" | "outline"; label?: string; scopes?: string[]; reauthOf?: string; oauthAvailable?: boolean; side?: IAdapter | null }) {
  const t = useT()
  const starts = startsOf(side), viaDevice = offers(starts, "device"), viaOAuth = offers(starts, "oauth") && (oauthAvailable || !viaDevice)
  const [form, setForm] = useState(false)
  const [busy, act] = useBusy(), [device, setDevice] = useState<RDevice | null>(null), [showDevice, setShowDevice] = useState(false)
  const [choose, setChoose] = useState(false), [sendAsMe, setSendAsMe] = useState(false), [confirmOAuth, setConfirmOAuth] = useState(false)
  const generation = useRef(0), scopeKey = scopes?.join(" ") ?? ""
  useEffect(() => { ++generation.current; setDevice(null); setShowDevice(false); setChoose(false); setConfirmOAuth(false); return () => { ++generation.current } }, [app, reauthOf, scopeKey])
  const oauth = async () => {
    const g = generation.current
    const r = await act(() => channelsApi.userOAuth(app, scopes))
    if (r?.url && g === generation.current) location.href = r.url
  }
  const start = async (requested?: string[]) => {
    const g = generation.current
    const r = await act(() => channelsApi.userDevice(app, requested, reauthOf))
    if (r && g === generation.current) { setDevice(r); setShowDevice(true); setChoose(false) }
  }
  const code = () => {
    if (device) { setShowDevice(true); return }
    if (reauthOf || scopes?.length) void start(scopes)
    else { setSendAsMe(false); setChoose(true) }
  }
  return <>
    {confirmOAuth && <Modal title={t("integrations:feishu.oauthTitle")} desc={t("integrations:feishu.oauthDesc")} onClose={() => setConfirmOAuth(false)} w={460}
      footer={<><Button variant="outline" disabled={busy} onClick={() => setConfirmOAuth(false)}>{t("common:action.back")}</Button><Button disabled={busy} onClick={() => void oauth()}>{busy && <Spinner />}{t("integrations:feishu.oauthGo")}</Button></>}><span className="type-footnote text-muted-foreground">{t("integrations:feishu.oauthNote")}</span></Modal>}
    {choose && <Modal title={t("integrations:feishu.chooseTitle")} desc={t("integrations:feishu.chooseDesc")} onClose={() => setChoose(false)} w={460}
      footer={<><Button variant="outline" disabled={busy} onClick={() => setChoose(false)}>{t("integrations:feishu.notNow")}</Button><Button disabled={busy} onClick={() => void start([...USER_READ_SCOPES, ...(sendAsMe ? ["im:message.send_as_user"] : [])])}>{busy && <Spinner />}{t("integrations:feishu.continueOfficial")}</Button></>}>
      <div className="type-callout">{t("integrations:feishu.readNote")}</div>
      <label className="flex cursor-pointer items-start gap-3 rounded-lg bg-muted/60 p-3 type-callout transition-colors duration-180 hover:bg-accent"><Checkbox className="mt-0.5" checked={sendAsMe} onCheckedChange={(v) => setSendAsMe(!!v)} /><span>{t("integrations:feishu.sendAsMe")}<span className="mt-1 block type-footnote text-muted-foreground">{t("integrations:feishu.sendAsMeNote")}</span></span></label>
    </Modal>}
    {device && showDevice && <DeviceDialog d={device} onClose={() => setShowDevice(false)} onFinished={() => { setDevice(null); setShowDevice(false) }} />}
    {form && side && <ConnectDialog adapter={asAdapter(side, "feishu")} onClose={() => setForm(false)} />}
    {viaDevice && <Button variant={variant} disabled={busy || disabled} onClick={code} data-self-device>{busy ? <Spinner /> : <ExternalLinkIcon />}{device ? t("integrations:feishu.continueSelf") : label ?? t("integrations:feishu.authorizeSelf")}</Button>}
    {viaOAuth && <Button variant={viaDevice ? "outline" : variant} disabled={busy || disabled} onClick={() => setConfirmOAuth(true)} data-self-oauth>{viaDevice ? t("integrations:feishu.useOAuth") : label ?? t("integrations:feishu.authorizeSelf")}</Button>}
    {offers(starts, "form") && side && <Button variant={variant} disabled={disabled} onClick={() => setForm(true)} data-self-form>{label ?? t("integrations:feishu.authorizeSelf")}</Button>}
    {routesOf(starts).map((r) => <Button key={r.href} variant={variant} disabled={disabled} nativeButton={false} render={<a href={r.href} />} data-self-route>{r.label}</Button>)}
  </>
}

function DeviceDialog({ d, onClose, onFinished }: { d: RDevice; onClose: () => void; onFinished: () => void }) {
  const t = useT()
  const [err, setErr] = useState(""), [terminal, setTerminal] = useState(false), [busy, setBusy] = useState(false)
  const version = useRef(0), mounted = useRef(true)
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; ++version.current } }, [])
  useEffect(() => {
    let stop = false, timer: ReturnType<typeof setTimeout>
    const tick = async () => {
      const v = version.current
      try {
        const s = await channelsApi.userDeviceStatus(d.id)
        if (stop || v !== version.current) return
        if (s.status === "ok") { toast(t("integrations:feishu.authorized")); onFinished(); go("#/channels/acc/" + s.account_id); return }
        if (s.status === "error") { setErr(s.error); setTerminal(true); return }
        if (s.status === "cancelled") { setErr(t("integrations:feishu.deviceCancelled")); setTerminal(true); return }
        setErr("")
      } catch (e) {
        if (stop || v !== version.current) return
        if (e instanceof HubError && e.status === 404) { setErr(t("integrations:feishu.deviceGone")); setTerminal(true); return }
        setErr(t("integrations:feishu.deviceRetrying"))
      }
      if (!stop) timer = setTimeout(tick, Math.max(2, d.interval) * 1000)
    }
    timer = setTimeout(tick, 0)
    return () => { stop = true; clearTimeout(timer) }
  }, [d.id, busy])
  const cancel = async () => {
    setBusy(true); ++version.current
    try {
      const s = await channelsApi.userDeviceCancel(d.id)
      if (!mounted.current) return
      if (s.status === "ok") { toast(t("integrations:feishu.cancelDoneFirst")); onFinished(); go("#/channels/acc/" + s.account_id) }
      else if (s.status === "cancelled") { toast(t("integrations:feishu.cancelStopped")); onFinished() }
      else if (s.status === "error") { setErr(s.error); setTerminal(true) }
      else setErr(t("integrations:feishu.cancelUnknown"))
    } catch (e) { report(e) }
    finally { ++version.current; if (mounted.current) setBusy(false) }
  }
  const url = officialConfirmationUrl(d.verification_uri_complete ?? d.verification_uri)
  return <Modal title={t("integrations:feishu.authorizeSelf")} desc={t("integrations:feishu.deviceDesc")} onClose={onClose} w={440}
    footer={terminal ? <Button onClick={onFinished}>{t("integrations:feishu.closeRestart")}</Button> : <><Button variant="outline" disabled={busy} onClick={() => void cancel()}>{t("integrations:feishu.cancelAuth")}</Button><Button variant="outline" onClick={onClose}>{t("integrations:feishu.later")}</Button></>}>
    <div className="flex flex-col items-center gap-3 py-2" data-device-flow>
      <div className="select-all rounded-xl bg-muted/60 px-6 py-3 type-display font-mono! tabular-nums tracking-[0.2em]!" aria-label={t("integrations:feishu.deviceCode")} data-device-code>{d.user_code}</div>
      {!terminal && url && <Button nativeButton={false} render={<a href={url} target="_blank" rel="noreferrer noopener" />}><ExternalLinkIcon />{t("integrations:feishu.openAuth")}</Button>}
      {!url && <p role="alert" className="type-callout text-destructive">{t("integrations:feishu.noAuthUrl")}</p>}
      <div className="type-footnote text-muted-foreground" role="status">{err ? <span className="text-destructive">{err}</span> : <span className="inline-flex items-center gap-1.5"><Spinner className="size-3" />{t("integrations:feishu.waiting")}</span>}</div>
      <p className="type-footnote text-muted-foreground">{t("integrations:feishu.cancelNote")}</p>
    </div>
  </Modal>
}

/* The default journey is official registration; manual configuration is a secondary path. */
type GuideAdapter = IAdapter

export function FeishuGuide({ adapters, bots, selfDone, callbackUrls }: { adapters: GuideAdapter[]; bots: { id: string; name: string; domain?: "feishu" | "lark" }[]; selfDone: boolean; callbackUrls: string[] }) {
  const t = useT()
  /* “授权你本人” only with a side acting as you that pairs with the bot; its buttons are the ways it declares */
  const side = selfSideOf(adapters) as IAdapter | null
  const [dlg, setDlg] = useState(false), [app, setApp] = useState(bots[0]?.id ?? "")
  const selected = bots.some((b) => b.id === app) ? app : bots[0]?.id ?? ""
  return <section className="flex flex-col gap-4" data-feishu-guide>
    {dlg && <FeishuConnectDialog bots={bots} onClose={() => setDlg(false)} side={side} />}
    <SectionLabel>{bots.length ? t("integrations:feishu.moreApps") : t("integrations:feishu.connectFeishuLark")}</SectionLabel>
    <FeishuRegistrationPanel />
    <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl bg-card p-5 shadow-[var(--lift)]">
      <div className="min-w-0"><div className="type-headline">{t("integrations:feishu.existingTitle")}</div><div className="mt-0.5 type-callout text-muted-foreground">{t("integrations:feishu.existingSub")}</div></div>
      <Button variant="outline" onClick={() => setDlg(true)} data-feishu-bot>{t("integrations:feishu.useExisting")}</Button>
    </div>
    {bots.length > 0 && side && <div className="flex flex-col gap-3 rounded-xl bg-card p-5 shadow-[var(--lift)]" data-self-optional>
      <div className="type-headline">{t("integrations:feishu.selfOptional")}</div>
      <p className="type-callout text-muted-foreground">{t("integrations:feishu.selfOptionalNote")}{selfDone && " " + t("integrations:feishu.selfDoneNote")}</p>
      <div className="flex flex-wrap items-center gap-2">
        {bots.length > 1 && <Select testId="guide-self-app" value={selected} onChange={setApp} options={bots.map((b) => ({ v: b.id, l: b.name }))} />}
        <SelfButtons app={selected} variant="outline" side={side} />
      </div>
    </div>}
    <Collapsible><CollapsibleTrigger className={DISC_TRIGGER + " text-left"}><DiscChevron />{t("integrations:feishu.advancedOAuth")}</CollapsibleTrigger>
      <CollapsibleContent><div className="pt-3 flex flex-col gap-2 type-footnote text-muted-foreground">
        <p>{t("integrations:feishu.oauthNoUrl")}</p>
        {callbackUrls.length ? <>
          <p>{t("integrations:feishu.callbackUrls")}</p>
          {callbackUrls.map((u) => <code key={u} className="select-all break-all rounded-lg bg-muted/60 px-3 py-2 type-code text-foreground" data-redirect-url>{u}</code>)}
          {selected && side && offers(startsOf(side), "oauth") && (bots.find((b) => b.id === selected)?.domain === "feishu" ? <SelfButtons app={selected} variant="outline" oauthAvailable side={side} /> : <p>{t("integrations:feishu.larkNoOAuth")}</p>)}
        </> : <p>{t("integrations:feishu.noCallback")}</p>}
        <p>{t("integrations:feishu.publicUrl")}</p>
      </div></CollapsibleContent>
    </Collapsible>
  </section>
}
