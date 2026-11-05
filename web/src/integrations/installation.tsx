/* One installation on the 接入 page (domain-model.md §1.3): a Feishu self-built app, a GitHub App, a mailbox. One card,
 * with the identities Steve works through — “Steve 以自己的身份” and “Steve 代你处理” — each with what it does, its
 * defaults, its capabilities and its permissions (一键申请 / 重新授权). Only Steve ever does the work; the identity
 * says in whose name. Per-group settings live on 渠道: this page links there. Feishu specifics come from `detail`. */
import { useState } from "react"
import { Trans } from "react-i18next"
import { ChevronRightIcon, ExternalLinkIcon, InfoIcon, RefreshCwIcon, TriangleAlertIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Switch } from "@/components/ui/switch"
import { Spinner } from "@/components/ui/spinner"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { go, Group, Row, StatusDot } from "@/ui"
import { Select } from "@/view-shared"
import { t as tr, useT } from "@/i18n"
import { tm, toast } from "@/store"
import { useBusy } from "@/accounts/load"
import { accountsApi, CONN_STATUS } from "@/api/accounts"
import { channelsApi, type ChannelAccount, type Rule, type Space } from "@/api/channels"
import { areaLabel, feishuGrantUrl, type FeishuAppDetail, type FeishuUserDetail, type IConnection, type IInstallation, type IProvider, type MissingRow } from "@/api/integrations"
import { ProviderIcon } from "@/integrations/index"
import { FeishuConnectDialog, SelfButtons } from "@/integrations/feishu"
import { CleanupDialog, DisconnectDialog, type Leaving } from "@/integrations/disconnect"
import { OwnerChip } from "../people/owner-chip"

const join = (xs: string[]) => xs.join(tr("integrations:list.sep"))
const isFeishu = (p: string) => p === "feishu"
const appOf = (c?: IConnection) => (c?.detail && "app_id" in c.detail ? (c.detail as FeishuAppDetail) : undefined)
const userOf = (c?: IConnection) => (c?.detail && "requested" in c.detail ? (c.detail as FeishuUserDetail) : undefined)
/** the missing rows of a connection: the Hub's detail, else the capability states */
function missingOf(c: IConnection): MissingRow[] {
  const d = (c.detail as { missing?: MissingRow[] } | undefined)?.missing
  if (d) return d
  return c.capabilities.flatMap((a) => a.missing_scopes.map((scope) => ({ scope, purpose: areaLabel(a.area), level: "required" as const, status: "missing" as const })))
}
const LEVELS = ["required", "suggested", "directory", "optional"] as const
const levelLabel = (l: string) => ((LEVELS as readonly string[]).includes(l) ? tr(`integrations:inst.level.${l as (typeof LEVELS)[number]}`) : "")

/* ───────── default roles (the identity's default rules; per-group rules live on 渠道) ───────── */
/** `v` names the preset (and its label key); the rules are what the Hub stores */
type Preset = { v: "reply" | "draft" | "record"; rules: Rule[] }
const BOT_PRESETS: Preset[] = [
  { v: "reply", rules: [{ when: { kind: "mention_or_dm" }, act: "reply" }] },
  { v: "draft", rules: [{ when: { kind: "mention_or_dm" }, act: "draft" }] },
  { v: "record", rules: [{ when: { kind: "all" }, act: "record" }] },
]
const SELF_PRESETS: Preset[] = [
  { v: "draft", rules: [{ when: { kind: "dm" }, act: "draft" }, { when: { kind: "mention" }, act: "draft" }, { when: { kind: "all" }, act: "record" }] },
  { v: "record", rules: [{ when: { kind: "all" }, act: "record" }] },
]
const norm = (rs: Rule[]) => JSON.stringify(rs.map((r) => ({ when: r.when, act: r.act })))
const presetOf = (ps: Preset[], rs: Rule[] | null): string | undefined => (rs ? ps.find((p) => norm(p.rules) === norm(rs))?.v ?? "custom" : undefined)

/** a setting that is on but can't work: said next to it, never silently */
const Blocked = ({ children, ...rest }: { children: React.ReactNode; [k: string]: any }) => (
  <div className="flex items-start gap-1.5 px-1 pt-1.5 type-footnote text-attention" role="note" {...rest}><TriangleAlertIcon className="mt-0.5 size-3.5 shrink-0" aria-hidden />{children}</div>
)
const Line = ({ children, className = "" }: { children: React.ReactNode; className?: string }) => <div className={"type-callout text-muted-foreground " + className}>{children}</div>

/* ───────── the card ───────── */
export function InstallationCard({ inst, p, chans, onChange }: { inst: IInstallation; p: IProvider; chans: ChannelAccount[] | null; onChange: () => void }) {
  const t = useT()
  const steve = inst.identities.find((i) => i.kind === "steve")?.connection, you = inst.identities.find((i) => i.kind === "you")?.connection
  const app = appOf(steve), [s, dot] = CONN_STATUS[inst.status] ?? [inst.status, "idle"]
  const feishu = isFeishu(p.id)
  const hasAdapter = (k: "steve" | "you") => p.adapters.some((a) => (a.identity === "bot" || a.identity === "app" ? "steve" : "you") === k)
  const [leave, setLeave] = useState<{ title: string; leaving: Leaving[] } | null>(null)
  const label = (k: "steve" | "you") => (k === "steve" ? (feishu ? t("integrations:inst.steveBot") : t("integrations:inst.steveSelf")) : t("integrations:inst.you"))
  const stops = (k: "steve" | "you") => (k === "steve" ? t("integrations:inst.stopsSteve") : t("integrations:inst.stopsYou"))
  const all = inst.identities.map((i) => ({ id: i.connection.id, label: label(i.kind), stops: stops(i.kind) }))
  const chanOf = (c?: IConnection) => (c ? chans?.find((a) => a.connection.id === c.id) ?? null : null)
  return (
    <section className="overflow-hidden rounded-2xl border bg-card" data-installation={inst.id} data-installation-missing={inst.missing} aria-label={inst.name}>
      {leave && <DisconnectDialog title={leave.title} name={inst.name} leaving={leave.leaving} onClose={() => setLeave(null)} onDone={() => { setLeave(null); onChange() }}
        keptNote={feishu ? t("integrations:inst.keptNoteFeishu") : undefined} />}
      <header className="flex items-start gap-3 border-b px-4 py-3.5">
        <ProviderIcon id={p.id} size={36} />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5"><span className="type-headline" data-installation-name>{inst.name}</span>
            {inst.missing > 0 && <Badge variant="outline" className="border-attention/40 font-normal text-attention" data-missing-badge>{t("integrations:inst.missingBadge", { count: inst.missing })}</Badge>}</div>
          <div className="mt-0.5 flex flex-wrap items-center gap-x-1.5 type-footnote text-muted-foreground">
            <span className="inline-flex items-center gap-1"><StatusDot s={dot} />{s}</span>
            <span className="text-tertiary">·</span><span>{feishu ? (app?.domain === "lark" ? t("integrations:inst.larkApp") : t("integrations:inst.feishuApp")) : p.name}</span>
            {app && <><span className="text-tertiary">·</span><span className="font-mono text-[12px]">{app.app_id}</span></>}
          </div>
        </div>
        {app && <a className="hidden shrink-0 items-center gap-1 rounded type-footnote text-blue hover:underline sm:inline-flex" href={app.console_url} target="_blank" rel="noreferrer noopener">{t("integrations:inst.console")}<ExternalLinkIcon className="size-3" /></a>}
      </header>
      {feishu && <div className="border-b bg-muted/30 px-4 py-2.5 type-footnote text-muted-foreground" data-who-works>
        {t("integrations:inst.whoWorks")}</div>}
      <div className="flex flex-col divide-y">
        {(["steve", "you"] as const).map((k) => {
          const c = k === "steve" ? steve : you
          if (c) return <Identity key={k} kind={k} c={c} p={p} chan={chanOf(c)} app={app} appConn={steve} onLeave={() => setLeave({ title: t("integrations:inst.disconnectWhat", { what: label(k) }), leaving: [{ id: c.id, label: label(k), stops: stops(k) }] })} />
          if (!hasAdapter(k)) return null
          return <Enable key={k} kind={k} p={p} inst={inst} />
        })}
      </div>
      {steve && you && feishu && <VoiceRule bot={chanOf(steve)} me={chanOf(you)} />}
      <footer className="flex flex-wrap items-center justify-between gap-2 border-t px-4 py-2.5">
        <Tech inst={inst} p={p} />
        {inst.identities.length > 1 && <Button size="sm" variant="ghost" className="text-destructive" onClick={() => setLeave({ title: t(feishu ? "integrations:inst.disconnectAllFeishu" : "integrations:inst.disconnectAllOther", { name: inst.name }), leaving: all })} data-disconnect-installation>{feishu ? t("integrations:inst.disconnectAllApp") : t("integrations:inst.disconnectAllConn")}</Button>}
      </footer>
    </section>
  )
}

/** a side not connected yet: what it would do, and the step to turn it on */
function Enable({ kind, p, inst }: { kind: "steve" | "you"; p: IProvider; inst: IInstallation }) {
  const t = useT()
  const [dlg, setDlg] = useState(false)
  const feishu = isFeishu(p.id)
  return (
    <div className="px-4 py-4" data-identity={kind} data-identity-state="off">
      {dlg && <FeishuConnectDialog bots={[]} onClose={() => setDlg(false)} />}
      <div className="flex flex-wrap items-center gap-2"><span className="type-headline">{kind === "steve" ? t("integrations:inst.titleSteveBot") : t("integrations:inst.titleYouFeishu")}</span><Badge variant="outline" className="font-normal text-muted-foreground">{t("integrations:inst.optionalOff")}</Badge></div>
      <Line className="mt-1">{kind === "you"
        ? t("integrations:inst.enableYou")
        : t("integrations:inst.enableSteve")}</Line>
      {feishu && <div className="mt-3 flex flex-wrap gap-2">{kind === "you"
        ? <SelfButtons app={inst.id} variant="outline" />
        : <Button size="sm" variant="outline" onClick={() => setDlg(true)} data-enable-bot>{t("integrations:inst.enableBot")}</Button>}</div>}
      {kind === "you" && feishu && <div className="mt-2 type-footnote text-muted-foreground">{t("integrations:inst.enableYouNote")}</div>}
    </div>
  )
}

/* ───────── one identity ───────── */
function Identity({ kind, c, p, chan, app, appConn, onLeave }: { kind: "steve" | "you"; c: IConnection; p: IProvider; chan: ChannelAccount | null; app?: FeishuAppDetail; appConn?: IConnection; onLeave: () => void }) {
  const t = useT()
  const feishu = isFeishu(p.id), [s, dot] = CONN_STATUS[c.status] ?? [c.status, "idle"], [clean, setClean] = useState(false)
  const name = c.name || c.handle
  const live = c.capabilities.filter((a) => a.state !== "planned"), planned = c.capabilities.filter((a) => a.state === "planned")
  const channelHref = live.some((a) => a.area === "channel") ? "#/channels/acc/" + c.id : "#/accounts/" + c.account_id
  const title = kind === "steve" ? (feishu ? t("integrations:inst.titleSteveBot") : t("integrations:inst.titleSteve")) : feishu ? t("integrations:inst.titleYouFeishu") : t("integrations:inst.titleYou")
  const what = feishu
    ? kind === "steve"
      ? t("integrations:inst.whatSteveFeishu", { name })
      : <Trans t={t} i18nKey="integrations:inst.whatYouFeishu" values={{ name }} components={{ code: <code className="font-mono text-[12px]" /> }} />
    : kind === "steve" ? t("integrations:inst.whatSteve", { name }) : t("integrations:inst.whatYou", { name })
  return (
    <div className="flex flex-col gap-3 px-4 py-4" data-identity={kind} data-identity-state="on" data-conn={c.id}>
      <div>
        <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5"><span className="type-headline">{title}</span>
          <span className="inline-flex items-center gap-1 type-footnote text-muted-foreground"><StatusDot s={dot} />{s}</span></div>
        <Line className="mt-1">{what}</Line>
      </div>
      {c.status === "reauth" && <Blocked data-blocked="reauth">{kind === "you" ? t("integrations:inst.reauthYou") : t("integrations:inst.reauthSteve")}</Blocked>}
      {c.status === "paused" && <Blocked data-blocked="paused">{t("integrations:inst.paused")}</Blocked>}
      {c.status === "error" && <Blocked data-blocked="error">{c.health.last_error ? t("integrations:inst.errorWith", { error: c.health.last_error }) : t("integrations:inst.errorPlain")}</Blocked>}
      <Group>
        {feishu && chan && <Defaults kind={kind} c={c} chan={chan} />}
        {kind === "steve" && feishu && chan?.persona && <Row title={t("integrations:inst.persona")} sub={t("integrations:inst.personaSub")} onClick={() => go("#/channels/personas/" + chan.persona!.id)} data-persona>
          <span className="type-callout text-muted-foreground">{chan.persona.name}</span><ChevronRightIcon className="size-4 text-tertiary" /></Row>}
        <Row title={feishu ? t("integrations:inst.perGroup") : t("integrations:inst.settings")} sub={feishu ? t("integrations:inst.perGroupSub") : undefined} onClick={() => go(channelHref)} data-identity-settings>
          <ChevronRightIcon className="size-4 text-tertiary" /></Row>
        <Row title={t("integrations:inst.owner")} data-conn-owner><OwnerChip objectType="connection" objectId={c.id} /></Row>
      </Group>
      <Capabilities c={c} kind={kind} feishu={feishu} planned={planned.map((a) => areaLabel(a.area))} />
      {feishu && (kind === "steve" ? <BotPermissions c={c} app={app} /> : <UserPermissions c={c} app={app} appConn={appConn} />)}
      {!feishu && missingOf(c).length > 0 && <Blocked>{t("integrations:inst.missingScopes", { list: join(missingOf(c).map((m) => m.scope)) })}</Blocked>}
      {c.warning && <Blocked data-blocked="warning">{c.warning}</Blocked>}
      {clean && <CleanupDialog target={{ connection_id: c.id }} name={name} onClose={() => setClean(false)} onDone={() => setClean(false)} />}
      <div className="flex flex-wrap gap-1">
        <Button size="sm" variant="ghost" className="-ml-2 text-destructive" onClick={onLeave} data-disconnect-identity={kind}>{kind === "steve" ? (feishu ? t("integrations:inst.disconnectBot") : t("integrations:inst.disconnectPlain")) : t("integrations:inst.disconnectYou")}</Button>
        {live.some((a) => a.area === "channel") && <Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => setClean(true)} data-cleanup-identity={kind}>{t("integrations:inst.cleanup")}</Button>}
      </div>
    </div>
  )
}

/** the identity's defaults: for groups without their own settings (new groups too); `/bot` for you */
function Defaults({ kind, c, chan }: { kind: "steve" | "you"; c: IConnection; chan: ChannelAccount }) {
  const t = useT()
  const [busy, act] = useBusy()
  const presets = kind === "steve" ? BOT_PRESETS : SELF_PRESETS
  const cur = presetOf(presets, chan.rules)
  const [v, setV] = useState(cur), [botOn, setBotOn] = useState(!!chan.trigger?.on)
  const caps = (area: string) => c.capabilities.find((a) => a.area === area)
  const sendAsYou = !!caps("send_as_you")?.caps.includes("msg.send_as_user")
  const noSend = kind === "you" && !sendAsYou
  const groupMsg = !(caps("channel")?.suggested_scopes ?? []).includes("im:message.group_msg")
  const save = async (nv: string) => {
    const pr = presets.find((x) => x.v === nv); if (!pr) return
    const old = v; setV(nv)
    if (!(await act(() => channelsApi.patchAccount(c.id, { rules: pr.rules }), t("common:state.saved")))) setV(old)
  }
  const toggleBot = async (on: boolean) => { setBotOn(on); if (!(await act(() => channelsApi.patchAccount(c.id, { trigger: { on } })))) setBotOn(!on) }
  const options = [...presets.map((x) => ({ v: x.v as string, l: t(`integrations:inst.preset.${x.v}`) })), ...(v === "custom" ? [{ v: "custom", l: t("integrations:inst.preset.custom") }] : [])]
  return <>
    <div data-default-role={v}>
      <Row title={kind === "steve" ? t("integrations:inst.defaultNew") : t("integrations:inst.defaultAny")}
        sub={kind === "steve" ? t("integrations:inst.defaultNewSub") : t("integrations:inst.defaultAnySub")}>
        {noSend ? <span className="type-callout text-muted-foreground" data-default-locked>{t("integrations:inst.recordOnly")}</span>
          : v !== undefined && <Select align="end" testId={"default-role-" + kind} value={v} onChange={(nv) => void save(nv)} options={options} />}{busy && <Spinner className="size-3" />}
      </Row>
      <div className="flex flex-col px-2 pb-2 type-footnote text-muted-foreground empty:hidden">
        {kind === "you" && sendAsYou && <span>{t("integrations:inst.replyPerGroup")}</span>}
        {noSend && <Blocked data-blocked="send_as_user">{t("integrations:inst.noSendAsUser")}</Blocked>}
        {kind === "steve" && v === "record" && !groupMsg && <Blocked data-blocked="group_msg">{t("integrations:inst.noGroupMsg")}</Blocked>}
        {!chan.editable && <span>{t("integrations:inst.notEditable")}</span>}
      </div>
    </div>
    {kind === "you" && <Row title={<span><Trans t={t} i18nKey="integrations:inst.botCommand" components={{ code: <code className="font-mono" /> }} /></span>} data-bot-trigger={botOn ? "on" : "off"}
      sub={<span className="flex flex-col"><span>{t("integrations:inst.botCommandSub")}</span>
        {botOn && c.status !== "ok" && <Blocked data-blocked="trigger">{t("integrations:inst.botCommandBlocked", { status: CONN_STATUS[c.status]?.[0] ?? c.status })}</Blocked>}
        {botOn && c.status === "ok" && noSend && <span data-trigger-web>{t("integrations:inst.botCommandWeb")}</span>}</span>}>
      <Switch checked={botOn} disabled={busy || !chan.editable} onCheckedChange={(on) => void toggleBot(on)} aria-label={t("integrations:inst.botCommandAria")} />
    </Row>}
  </>
}

/** the live capabilities of an identity, and the planned ones in one line */
function Capabilities({ c, kind, feishu, planned }: { c: IConnection; kind: "steve" | "you"; feishu: boolean; planned: string[] }) {
  const t = useT()
  const rows = missingOf(c), purpose = (scope: string) => rows.find((r) => r.scope === scope)?.purpose || scope
  const live = c.capabilities.filter((a) => a.state !== "planned")
  const label = (a: IConnection["capabilities"][number]): [string, string] =>
    a.state === "missing" ? [t("integrations:inst.cap.missing"), "wait"] : a.state === "off" ? [t("integrations:inst.cap.off"), "idle"] : c.status !== "ok" ? [t("integrations:inst.cap.unavailable"), "wait"] : [t("integrations:inst.cap.on"), "done"]
  const NOTE: Record<string, string> = feishu ? { channel: kind === "steve" ? t("integrations:inst.cap.channelSteve") : t("integrations:inst.cap.channelYou"), send_as_you: t("integrations:inst.cap.sendAsYou"), directory: t("integrations:inst.cap.directory") } : {}
  return (
    <div data-capabilities={kind}>
      <div className="mb-1.5 px-1 type-footnote font-medium text-muted-foreground">{t("integrations:inst.cap.title")}</div>
      <ul className="flex flex-col gap-1.5">{live.map((a) => { const [l, d] = label(a); return (
        <li key={a.area} className="flex items-start gap-2 px-1" data-cap-area={a.area} data-cap-state={a.state}>
          <StatusDot s={d} className="mt-1.5" />
          <div className="min-w-0 flex-1 type-callout"><span>{areaLabel(a.area)}</span><span className="text-muted-foreground"> · {NOTE[a.area] ?? a.note ?? ""}</span>
            {a.state === "missing" && <div className="type-footnote text-attention">{t(a.caps.length ? "integrations:inst.cap.missingSome" : "integrations:inst.cap.missingAll", { list: join(a.missing_scopes.map(purpose)) })}</div>}
          </div>
          <span className="shrink-0 type-footnote text-muted-foreground">{l}</span>
        </li>) })}</ul>
      {planned.length > 0 && <div className="mt-1.5 px-1 type-footnote text-muted-foreground" data-planned-line>{t("integrations:inst.cap.planned", { list: join(planned) })}</div>}
    </div>
  )
}

function MissingList({ rows }: { rows: MissingRow[] }) {
  return (
    <ul className="flex flex-col gap-1" data-missing-list>{rows.map((r) => (
      <li key={r.scope} className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5" data-missing={r.scope}>
        <span className="type-callout">{r.purpose}</span>
        <span className="type-footnote text-muted-foreground">{levelLabel(r.level)}{r.refused_at ? " · " + tr("integrations:inst.refusedAt", { time: tm(r.refused_at) }) : ""}</span>
      </li>))}</ul>
  )
}

/** the bot's permissions: the one link that preselects all missing ones, publish, check again */
function BotPermissions({ c, app }: { c: IConnection; app?: FeishuAppDetail }) {
  const t = useT()
  const [busy, act] = useBusy()
  const rows = missingOf(c), grant = app?.grant?.url ?? (app && rows.length ? feishuGrantUrl(app.domain, app.app_id, rows.map((r) => r.scope), true) : undefined)
  const check = async () => {
    const r = await act(() => accountsApi.test(c.id))
    if (r) toast(r.status === "ok" ? t("integrations:inst.rechecked") : t("integrations:inst.checkedStatus", { status: CONN_STATUS[r.status]?.[0] ?? r.status }))
  }
  return (
    <div className="flex flex-col gap-2 rounded-xl bg-muted/40 px-3.5 py-3" data-permissions-block="bot" data-missing-count={rows.length}>
      <div className="type-callout font-medium">{rows.length ? t("integrations:inst.botMissing", { count: rows.length }) : t("integrations:inst.botPerms")}</div>
      {rows.length ? <>
        <MissingList rows={rows} />
        <ol className="flex flex-col gap-2 type-callout">
          <li className="flex flex-wrap items-center gap-2"><span className="text-muted-foreground">1.</span>
            {grant ? <Button size="sm" nativeButton={false} render={<a href={grant} target="_blank" rel="noreferrer noopener" data-grant-link={app?.grant?.from ?? "steve"} />}><ExternalLinkIcon />{t("integrations:inst.grant")}</Button> : <span className="text-muted-foreground">{t("integrations:inst.grantManual")}</span>}
            <span className="type-footnote text-muted-foreground">{t("integrations:inst.grantNote")}</span></li>
          <li className="flex flex-wrap items-center gap-2"><span className="text-muted-foreground">2.</span>
            {app ? <a className="inline-flex items-center gap-1 text-blue hover:underline" href={app.publish_url} target="_blank" rel="noreferrer noopener" data-publish-link>{t("integrations:inst.publish")}<ExternalLinkIcon className="size-3" /></a> : <span>{t("integrations:inst.publish")}</span>}
            <span className="type-footnote text-muted-foreground">{t("integrations:inst.publishNote")}</span></li>
          <li className="flex flex-wrap items-center gap-2"><span className="text-muted-foreground">3.</span>
            <Button size="sm" variant="outline" disabled={busy} onClick={() => void check()} data-recheck>{busy ? <Spinner /> : <RefreshCwIcon />}{t("integrations:inst.recheckDone")}</Button></li>
        </ol>
      </> : <div className="flex flex-wrap items-center gap-2">
        <span className="type-footnote text-muted-foreground">{app?.checked_at ? t("integrations:inst.allGrantedAt", { time: tm(app.checked_at) }) : t("integrations:inst.allGranted")}</span>
        <Button size="sm" variant="ghost" className="-ml-1" disabled={busy} onClick={() => void check()} data-recheck>{busy ? <Spinner /> : <RefreshCwIcon />}{t("integrations:inst.recheck")}</Button></div>}
    </div>
  )
}

/** your permissions: re-authorize with the missing scopes; the console link when the app hasn't enabled them */
function UserPermissions({ c, app, appConn }: { c: IConnection; app?: FeishuAppDetail; appConn?: IConnection }) {
  const t = useT()
  const rows = missingOf(c), d = userOf(c), via = d?.app_connection ?? appConn?.id
  const need = rows.length > 0 || c.status === "reauth"
  if (!need) return <div className="px-1 type-footnote text-muted-foreground" data-permissions-block="you" data-missing-count={0}>{t("integrations:inst.youAllGranted")}</div>
  return (
    <div className="flex flex-col gap-2 rounded-xl bg-muted/40 px-3.5 py-3" data-permissions-block="you" data-missing-count={rows.length}>
      <div className="type-callout font-medium">{rows.length ? t("integrations:inst.youMissing", { count: rows.length }) : t("integrations:inst.reauthExpired")}</div>
      {rows.length > 0 && <MissingList rows={rows} />}
      <div className="flex flex-wrap items-center gap-2">
        <SelfButtons app={via ?? ""} disabled={!via} label={t("integrations:inst.reauth")} scopes={rows.map((x) => x.scope)} reauthOf={c.id} />
        <span className="type-footnote text-muted-foreground">{rows.length ? t("integrations:inst.reauthMissing") : t("integrations:inst.reauthToken")}</span>
      </div>
      {rows.length > 0 && app && <div className="type-footnote text-muted-foreground"><Trans t={t} i18nKey="integrations:inst.consoleHint" components={{ a: <a className="text-blue hover:underline" href={feishuGrantUrl(app.domain, app.app_id, rows.map((x) => x.scope), false)} target="_blank" rel="noreferrer noopener" data-user-grant-link /> }} /></div>}
    </div>
  )
}

/** one speaker per group: the bot where it is, you elsewhere (domain-model.md §1.4) */
function VoiceRule({ bot, me }: { bot: ChannelAccount | null; me: ChannelAccount | null }) {
  const t = useT()
  const groups = (a: ChannelAccount | null) => (a?.spaces ?? []).filter((s) => s.status === "active" && s.kind !== "dm")
  // one space per chat: a group both identities see is the same space in both lists
  const mine = groups(me), shared: Space[] = groups(bot).filter((s) => mine.some((m) => m.id === s.id))
  return (
    <div className="flex gap-2.5 border-t bg-muted/30 px-4 py-3" data-voice-rule data-shared-groups={shared.length}>
      <InfoIcon className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
      <div className="min-w-0 flex-1 type-callout">
        <div className="font-medium">{t("integrations:inst.voiceTitle")}</div>
        <div className="text-muted-foreground">{t("integrations:inst.voiceBody")}</div>
        {shared.length > 0 && <div className="mt-1.5 flex flex-wrap items-center gap-x-2 gap-y-1 type-footnote text-muted-foreground"><span>{t("integrations:inst.sharedGroups")}</span>
          {shared.slice(0, 6).map((s) => <a key={s.id} className="text-blue hover:underline" href={"#/channels/space/" + s.id} data-shared-group>{s.name}</a>)}
          {shared.length > 6 && <span>{t("integrations:inst.andMore", { count: shared.length })}</span>}</div>}
      </div>
    </div>
  )
}

/** what a technical reader wants, out of the way */
function Tech({ inst, p }: { inst: IInstallation; p: IProvider }) {
  const t = useT()
  const feishu = isFeishu(p.id)
  return (
    <Collapsible className="min-w-0 flex-1">
      <CollapsibleTrigger className="rounded type-footnote text-muted-foreground hover:text-foreground hover:underline" data-tech-toggle>{t("integrations:inst.tech")}</CollapsibleTrigger>
      <CollapsibleContent className="mt-2 flex flex-col gap-2 type-footnote text-muted-foreground" data-tech>
        {inst.identities.map(({ kind, connection: c }) => { const a = appOf(c), u = userOf(c); return (
          <dl key={c.id} className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5">
            <dt>{kind === "steve" ? t("integrations:inst.botConn") : t("integrations:inst.yourConn")}</dt><dd className="break-all font-mono text-[12px]">{c.id} · {c.adapter}</dd>
            {a && <><dt>App ID</dt><dd className="break-all font-mono text-[12px]">{a.app_id}</dd></>}
            {feishu && <><dt>{t("integrations:inst.token")}</dt><dd>{kind === "steve" ? t("integrations:inst.tokenSteve") : t("integrations:inst.tokenYou")}</dd></>}
            {u && <><dt>{t("integrations:inst.requested")}</dt><dd className="break-all font-mono text-[12px]">{u.requested.join(" ")}</dd></>}
            <dt>{t("integrations:inst.lastOk")}</dt><dd>{c.health.last_ok ? tm(c.health.last_ok) : "—"}</dd>
            {c.health.last_error && <><dt>{t("integrations:inst.lastError")}</dt><dd className="break-all">{c.health.last_error}</dd></>}
            {a?.grant && <><dt>{t("integrations:inst.grantLink")}</dt><dd className="break-all font-mono text-[12px]">{t(a.grant.from === "feishu" ? "integrations:inst.grantFromFeishu" : "integrations:inst.grantFromSteve", { url: a.grant.url })}</dd></>}
          </dl>) })}
      </CollapsibleContent>
    </Collapsible>
  )
}
