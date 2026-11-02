/* One installation on the 接入 page (domain-model.md §1.3): a Feishu self-built app, a GitHub App, a mailbox. One card,
 * with the identities Steve works through — “Steve 以自己的身份” and “Steve 代你处理” — each with what it does, its
 * defaults, its capabilities and its permissions (一键申请 / 重新授权). Only Steve ever does the work; the identity
 * says in whose name. Per-group settings live on 渠道: this page links there. Feishu specifics come from `detail`. */
import { useState } from "react"
import { ChevronRightIcon, ExternalLinkIcon, InfoIcon, RefreshCwIcon, TriangleAlertIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Switch } from "@/components/ui/switch"
import { Spinner } from "@/components/ui/spinner"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { go, Group, Row, StatusDot } from "@/ui"
import { Select } from "@/views"
import { tm, toast } from "@/store"
import { useBusy } from "@/accounts/load"
import { accountsApi, CONN_STATUS } from "@/api/accounts"
import { channelsApi, type ChannelAccount, type Rule, type Space } from "@/api/channels"
import { areaLabel, feishuGrantUrl, type FeishuAppDetail, type FeishuUserDetail, type IConnection, type IInstallation, type IProvider, type MissingRow } from "@/api/integrations"
import { ProviderIcon } from "@/integrations/index"
import { FeishuConnectDialog, SelfButtons } from "@/integrations/feishu"
import { CleanupDialog, DisconnectDialog, type Leaving } from "@/integrations/disconnect"
import { OwnerChip } from "../people/owner-chip"

const join = (xs: string[]) => xs.join("、")
const isFeishu = (p: string) => p === "feishu"
const appOf = (c?: IConnection) => (c?.detail && "app_id" in c.detail ? (c.detail as FeishuAppDetail) : undefined)
const userOf = (c?: IConnection) => (c?.detail && "requested" in c.detail ? (c.detail as FeishuUserDetail) : undefined)
/** the missing rows of a connection: the Hub's detail, else the capability states */
function missingOf(c: IConnection): MissingRow[] {
  const d = (c.detail as { missing?: MissingRow[] } | undefined)?.missing
  if (d) return d
  return c.capabilities.flatMap((a) => a.missing_scopes.map((scope) => ({ scope, purpose: areaLabel(a.area), level: "required" as const, status: "missing" as const })))
}
const LEVEL: Record<string, string> = { required: "必须", suggested: "建议", directory: "通讯录", optional: "可选" }

/* ───────── default roles (the identity's default rules; per-group rules live on 渠道) ───────── */
type Preset = { v: string; l: string; rules: Rule[] }
const BOT_PRESETS: Preset[] = [
  { v: "reply", l: "被 @ 或私聊时回答", rules: [{ when: { kind: "mention_or_dm" }, act: "reply" }] },
  { v: "draft", l: "起草，等你批准", rules: [{ when: { kind: "mention_or_dm" }, act: "draft" }] },
  { v: "record", l: "只记录", rules: [{ when: { kind: "all" }, act: "record" }] },
]
const SELF_PRESETS: Preset[] = [
  { v: "draft", l: "起草，等你批准", rules: [{ when: { kind: "dm" }, act: "draft" }, { when: { kind: "mention" }, act: "draft" }, { when: { kind: "all" }, act: "record" }] },
  { v: "record", l: "只记录", rules: [{ when: { kind: "all" }, act: "record" }] },
]
const norm = (rs: Rule[]) => JSON.stringify(rs.map((r) => ({ when: r.when, act: r.act })))
const presetOf = (ps: Preset[], rs: Rule[] | null) => (rs ? ps.find((p) => norm(p.rules) === norm(rs))?.v ?? "custom" : undefined)

/** a setting that is on but can't work: said next to it, never silently */
const Blocked = ({ children, ...rest }: { children: React.ReactNode; [k: string]: any }) => (
  <div className="flex items-start gap-1.5 px-1 pt-1.5 type-footnote text-attention" role="note" {...rest}><TriangleAlertIcon className="mt-0.5 size-3.5 shrink-0" aria-hidden />{children}</div>
)
const Line = ({ children, className = "" }: { children: React.ReactNode; className?: string }) => <div className={"type-callout text-muted-foreground " + className}>{children}</div>

/* ───────── the card ───────── */
export function InstallationCard({ inst, p, chans, onChange }: { inst: IInstallation; p: IProvider; chans: ChannelAccount[] | null; onChange: () => void }) {
  const steve = inst.identities.find((i) => i.kind === "steve")?.connection, you = inst.identities.find((i) => i.kind === "you")?.connection
  const app = appOf(steve), [s, dot] = CONN_STATUS[inst.status] ?? [inst.status, "idle"]
  const feishu = isFeishu(p.id)
  const hasAdapter = (k: "steve" | "you") => p.adapters.some((a) => (a.identity === "bot" || a.identity === "app" ? "steve" : "you") === k)
  const [leave, setLeave] = useState<{ title: string; leaving: Leaving[] } | null>(null)
  const label = (k: "steve" | "you") => (k === "steve" ? (feishu ? "Steve 机器人" : "Steve 自己的身份") : "代你处理")
  const stops = (k: "steve" | "you") => (k === "steve" ? "不再在群里和私聊里收发消息" : "不再读你的消息，不再起草或回复")
  const all = inst.identities.map((i) => ({ id: i.connection.id, label: label(i.kind), stops: stops(i.kind) }))
  const chanOf = (c?: IConnection) => (c ? chans?.find((a) => a.connection.id === c.id) ?? null : null)
  return (
    <section className="overflow-hidden rounded-2xl border bg-card" data-installation={inst.id} data-installation-missing={inst.missing} aria-label={inst.name}>
      {leave && <DisconnectDialog title={leave.title} name={inst.name} leaving={leave.leaving} onClose={() => setLeave(null)} onDone={() => { setLeave(null); onChange() }}
        keptNote={feishu ? "飞书开发者后台里的应用本身不受影响" : undefined} />}
      <header className="flex items-start gap-3 border-b px-4 py-3.5">
        <ProviderIcon id={p.id} size={36} />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5"><span className="type-headline" data-installation-name>{inst.name}</span>
            {inst.missing > 0 && <Badge variant="outline" className="border-attention/40 font-normal text-attention" data-missing-badge>缺 {inst.missing} 项权限</Badge>}</div>
          <div className="mt-0.5 flex flex-wrap items-center gap-x-1.5 type-footnote text-muted-foreground">
            <span className="inline-flex items-center gap-1"><StatusDot s={dot} />{s}</span>
            <span className="text-tertiary">·</span><span>{feishu ? (app?.domain === "lark" ? "Lark 企业自建应用" : "飞书企业自建应用") : p.name}</span>
            {app && <><span className="text-tertiary">·</span><span className="font-mono text-[12px]">{app.app_id}</span></>}
          </div>
        </div>
        {app && <a className="hidden shrink-0 items-center gap-1 rounded type-footnote text-blue hover:underline sm:inline-flex" href={app.console_url} target="_blank" rel="noreferrer noopener">开发者后台<ExternalLinkIcon className="size-3" /></a>}
      </header>
      {feishu && <div className="border-b bg-muted/30 px-4 py-2.5 type-footnote text-muted-foreground" data-who-works>
        只有 Steve 在干活。这一个应用给了 Steve 两种身份：用机器人的身份在群里说话，或者用你的身份帮你看消息、起草和回复。</div>}
      <div className="flex flex-col divide-y">
        {(["steve", "you"] as const).map((k) => {
          const c = k === "steve" ? steve : you
          if (c) return <Identity key={k} kind={k} c={c} p={p} chan={chanOf(c)} app={app} appConn={steve} onLeave={() => setLeave({ title: `断开${label(k)}`, leaving: [{ id: c.id, label: label(k), stops: stops(k) }] })} />
          if (!hasAdapter(k)) return null
          return <Enable key={k} kind={k} p={p} inst={inst} />
        })}
      </div>
      {steve && you && feishu && <VoiceRule bot={chanOf(steve)} me={chanOf(you)} />}
      <footer className="flex flex-wrap items-center justify-between gap-2 border-t px-4 py-2.5">
        <Tech inst={inst} p={p} />
        {inst.identities.length > 1 && <Button size="sm" variant="ghost" className="text-destructive" onClick={() => setLeave({ title: `断开整个${feishu ? "飞书应用" : "接入"}“${inst.name}”`, leaving: all })} data-disconnect-installation>断开整个{feishu ? "应用" : "接入"}</Button>}
      </footer>
    </section>
  )
}

/** a side not connected yet: what it would do, and the step to turn it on */
function Enable({ kind, p, inst }: { kind: "steve" | "you"; p: IProvider; inst: IInstallation }) {
  const [dlg, setDlg] = useState(false)
  const feishu = isFeishu(p.id)
  return (
    <div className="px-4 py-4" data-identity={kind} data-identity-state="off">
      {dlg && <FeishuConnectDialog bots={[]} onClose={() => setDlg(false)} />}
      <div className="flex flex-wrap items-center gap-2"><span className="type-headline">{kind === "steve" ? "Steve 以自己的身份（机器人）" : "Steve 代你处理（以你的名义）"}</span><Badge variant="outline" className="font-normal text-muted-foreground">可选 · 未开启</Badge></div>
      <Line className="mt-1">{kind === "you"
        ? "开启后，Steve 用你的账号读你的私聊和群消息，按每个群的设置只记录、起草等你批准，或直接回复（开头带 [AI]）。不开也不影响机器人。"
        : "开启后，Steve 以机器人的名义在它所在的群里回答 @它的消息和私聊。代你处理不受影响。"}</Line>
      {feishu && <div className="mt-3 flex flex-wrap gap-2">{kind === "you"
        ? <SelfButtons app={inst.id} variant="outline" />
        : <Button size="sm" variant="outline" onClick={() => setDlg(true)} data-enable-bot>开启机器人</Button>}</div>}
      {kind === "you" && feishu && <div className="mt-2 type-footnote text-muted-foreground">在飞书里同意授权；之后可以随时在这里断开。</div>}
    </div>
  )
}

/* ───────── one identity ───────── */
function Identity({ kind, c, p, chan, app, appConn, onLeave }: { kind: "steve" | "you"; c: IConnection; p: IProvider; chan: ChannelAccount | null; app?: FeishuAppDetail; appConn?: IConnection; onLeave: () => void }) {
  const feishu = isFeishu(p.id), [s, dot] = CONN_STATUS[c.status] ?? [c.status, "idle"], [clean, setClean] = useState(false)
  const name = c.name || c.handle
  const live = c.capabilities.filter((a) => a.state !== "planned"), planned = c.capabilities.filter((a) => a.state === "planned")
  const channelHref = live.some((a) => a.area === "channel") ? "#/channels/acc/" + c.id : "#/accounts/" + c.account_id
  const title = kind === "steve" ? (feishu ? "Steve 以自己的身份（机器人）" : "Steve 以自己的身份") : feishu ? "Steve 代你处理（以你的名义）" : "Steve 代你处理"
  const what = feishu
    ? kind === "steve"
      ? <>在它所在的群里被 @、或者有人私聊它时，Steve 以「{name}」的名义回答。群里看到的发送者是这个机器人。</>
      : <>Steve 用你（{name}）的账号读你的私聊和群消息，大约 1 分钟一次。每个群可以设成只记录、起草等你批准，或直接回复（开头带 [AI]）。你在任何群里发 <code className="font-mono text-[12px]">/bot</code>，就是在给 Steve 下指令。</>
    : kind === "steve" ? <>Steve 以「{name}」的身份工作。</> : <>Steve 用你的账号「{name}」工作，只做你授权的事。</>
  return (
    <div className="flex flex-col gap-3 px-4 py-4" data-identity={kind} data-identity-state="on" data-conn={c.id}>
      <div>
        <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5"><span className="type-headline">{title}</span>
          <span className="inline-flex items-center gap-1 type-footnote text-muted-foreground"><StatusDot s={dot} />{s}</span></div>
        <Line className="mt-1">{what}</Line>
      </div>
      {c.status === "reauth" && <Blocked data-blocked="reauth">{kind === "you" ? "授权过期了：代你处理现在不生效，读不到消息也发不出去。点下面的“重新授权”。" : "飞书不认这个应用的凭据了：机器人现在不生效。去账号详情重新填写 App Secret。"}</Blocked>}
      {c.status === "paused" && <Blocked data-blocked="paused">已暂停：下面的设置现在都不生效。在账号详情里恢复。</Blocked>}
      {c.status === "error" && <Blocked data-blocked="error">最近连续出错{c.health.last_error ? `（${c.health.last_error}）` : ""}，Steve 会自动重试。</Blocked>}
      <Group>
        {feishu && chan && <Defaults kind={kind} c={c} chan={chan} />}
        {kind === "steve" && feishu && chan?.persona && <Row title="对外身份" sub="机器人在群里说话时用的名字、语气和署名；每个群可以单独换" onClick={() => go("#/channels/personas/" + chan.persona!.id)} data-persona>
          <span className="type-callout text-muted-foreground">{chan.persona.name}</span><ChevronRightIcon className="size-4 text-tertiary" /></Row>}
        <Row title={feishu ? "每个群怎么处理" : "设置"} sub={feishu ? "在“渠道”里按群设置：关注、角色、对外身份" : undefined} onClick={() => go(channelHref)} data-identity-settings>
          <ChevronRightIcon className="size-4 text-tertiary" /></Row>
        <Row title="负责人" data-conn-owner><OwnerChip objectType="connection" objectId={c.id} /></Row>
      </Group>
      <Capabilities c={c} kind={kind} feishu={feishu} planned={planned.map((a) => areaLabel(a.area))} />
      {feishu && (kind === "steve" ? <BotPermissions c={c} app={app} /> : <UserPermissions c={c} app={app} appConn={appConn} />)}
      {!feishu && missingOf(c).length > 0 && <Blocked>缺权限：{join(missingOf(c).map((m) => m.scope))}。到账号详情重新授权。</Blocked>}
      {c.warning && <Blocked data-blocked="warning">{c.warning}</Blocked>}
      {clean && <CleanupDialog target={{ connection_id: c.id }} name={name} onClose={() => setClean(false)} onDone={() => setClean(false)} />}
      <div className="flex flex-wrap gap-1">
        <Button size="sm" variant="ghost" className="-ml-2 text-destructive" onClick={onLeave} data-disconnect-identity={kind}>断开{kind === "steve" ? (feishu ? "机器人" : "") : "代你处理"}</Button>
        {live.some((a) => a.area === "channel") && <Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => setClean(true)} data-cleanup-identity={kind}>清理记录</Button>}
      </div>
    </div>
  )
}

/** the identity's defaults: for groups without their own settings (new groups too); `/bot` for you */
function Defaults({ kind, c, chan }: { kind: "steve" | "you"; c: IConnection; chan: ChannelAccount }) {
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
    if (!(await act(() => channelsApi.patchAccount(c.id, { rules: pr.rules }), "已保存"))) setV(old)
  }
  const toggleBot = async (on: boolean) => { setBotOn(on); if (!(await act(() => channelsApi.patchAccount(c.id, { trigger: { on } })))) setBotOn(!on) }
  const options = [...presets.map((x) => ({ v: x.v, l: x.l })), ...(v === "custom" ? [{ v: "custom", l: "自定义（在渠道里设置）" }] : [])]
  return <>
    <div data-default-role={v}>
      <Row title={kind === "steve" ? "在新群里默认怎么做" : "默认怎么做"}
        sub={kind === "steve" ? "由机器人说话的群里，新群和没单独设置的" : "由你说话的群（机器人不在的）和私聊里，没单独设置的"}>
        {noSend ? <span className="type-callout text-muted-foreground" data-default-locked>只记录</span>
          : v !== undefined && <Select align="end" testId={"default-role-" + kind} value={v} onChange={(nv) => void save(nv)} options={options} />}{busy && <Spinner className="size-3" />}
      </Row>
      <div className="flex flex-col px-2 pb-2 type-footnote text-muted-foreground empty:hidden">
        {kind === "you" && sendAsYou && <span>直接回复（开头带 [AI]）在“渠道”里按群打开。</span>}
        {noSend && <Blocked data-blocked="send_as_user">没有“以你本人身份发送”（im:message.send_as_user）的授权，所以 Steve 不替你起草或回复，只记录。有些飞书组织不开放申请这个权限；能申请的话，开通后点下面的“重新授权”。</Blocked>}
        {kind === "steve" && v === "record" && !groupMsg && <Blocked data-blocked="group_msg">只记录群里所有消息要开通“收群里所有消息”的权限；现在只能记到 @机器人 的消息。</Blocked>}
        {!chan.editable && <span>这个 Hub 还不能在这里改。</span>}
      </div>
    </div>
    {kind === "you" && <Row title={<span><code className="font-mono">/bot</code> 指令</span>} data-bot-trigger={botOn ? "on" : "off"}
      sub={<span className="flex flex-col"><span>你在任何群或私聊里发 /bot 开头的话，就是让 Steve 去办；它会在那里回复你</span>
        {botOn && c.status !== "ok" && <Blocked data-blocked="trigger">开着但现在不生效：{CONN_STATUS[c.status]?.[0] ?? c.status}。</Blocked>}
        {botOn && c.status === "ok" && noSend && <span data-trigger-web>没有以你本人身份发送的授权：/bot 的回答在 Steve 的对话里，不会发到群里。</span>}</span>}>
      <Switch checked={botOn} disabled={busy || !chan.editable} onCheckedChange={(on) => void toggleBot(on)} aria-label="/bot 指令" />
    </Row>}
  </>
}

/** the live capabilities of an identity, and the planned ones in one line */
function Capabilities({ c, kind, feishu, planned }: { c: IConnection; kind: "steve" | "you"; feishu: boolean; planned: string[] }) {
  const rows = missingOf(c), purpose = (scope: string) => rows.find((r) => r.scope === scope)?.purpose || scope
  const live = c.capabilities.filter((a) => a.state !== "planned")
  const label = (a: IConnection["capabilities"][number]): [string, string] =>
    a.state === "missing" ? ["缺权限", "wait"] : a.state === "off" ? ["已关闭", "idle"] : c.status !== "ok" ? ["暂不可用", "wait"] : ["已开启", "done"]
  const NOTE: Record<string, string> = feishu ? { channel: kind === "steve" ? "在群里和私聊里收发消息" : "读你的私聊和群消息", send_as_you: "以你本人身份发出你批准的回复", directory: "同步部门和成员到联系人" } : {}
  return (
    <div data-capabilities={kind}>
      <div className="mb-1.5 px-1 type-footnote font-medium text-muted-foreground">能做什么</div>
      <ul className="flex flex-col gap-1.5">{live.map((a) => { const [l, d] = label(a); return (
        <li key={a.area} className="flex items-start gap-2 px-1" data-cap-area={a.area} data-cap-state={a.state}>
          <StatusDot s={d} className="mt-1.5" />
          <div className="min-w-0 flex-1 type-callout"><span>{areaLabel(a.area)}</span><span className="text-muted-foreground"> · {NOTE[a.area] ?? a.note ?? ""}</span>
            {a.state === "missing" && <div className="type-footnote text-attention">{a.caps.length ? "部分不生效" : "开着但不生效"}：缺“{join(a.missing_scopes.map(purpose))}”的权限，见下面</div>}
          </div>
          <span className="shrink-0 type-footnote text-muted-foreground">{l}</span>
        </li>) })}</ul>
      {planned.length > 0 && <div className="mt-1.5 px-1 type-footnote text-muted-foreground" data-planned-line>规划中：{join(planned)}</div>}
    </div>
  )
}

function MissingList({ rows }: { rows: MissingRow[] }) {
  return (
    <ul className="flex flex-col gap-1" data-missing-list>{rows.map((r) => (
      <li key={r.scope} className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5" data-missing={r.scope}>
        <span className="type-callout">{r.purpose}</span>
        <span className="type-footnote text-muted-foreground">{LEVEL[r.level] ?? ""}{r.refused_at ? ` · ${tm(r.refused_at)} 工作时被飞书拒绝` : ""}</span>
      </li>))}</ul>
  )
}

/** the bot's permissions: the one link that preselects all missing ones, publish, check again */
function BotPermissions({ c, app }: { c: IConnection; app?: FeishuAppDetail }) {
  const [busy, act] = useBusy()
  const rows = missingOf(c), grant = app?.grant?.url ?? (app && rows.length ? feishuGrantUrl(app.domain, app.app_id, rows.map((r) => r.scope), true) : undefined)
  const check = async () => {
    const r = await act(() => accountsApi.test(c.id))
    if (r) toast(r.status === "ok" ? "已重新检查" : `检查完了：${CONN_STATUS[r.status]?.[0] ?? r.status}`)
  }
  return (
    <div className="flex flex-col gap-2 rounded-xl bg-muted/40 px-3.5 py-3" data-permissions-block="bot" data-missing-count={rows.length}>
      <div className="type-callout font-medium">{rows.length ? `机器人缺 ${rows.length} 项权限` : "机器人的权限"}</div>
      {rows.length ? <>
        <MissingList rows={rows} />
        <ol className="flex flex-col gap-2 type-callout">
          <li className="flex flex-wrap items-center gap-2"><span className="text-muted-foreground">1.</span>
            {grant ? <Button size="sm" nativeButton={false} render={<a href={grant} target="_blank" rel="noreferrer noopener" data-grant-link={app?.grant?.from ?? "steve"} />}><ExternalLinkIcon />一键申请权限</Button> : <span className="text-muted-foreground">在开发者后台的权限管理里开通上面几项</span>}
            <span className="type-footnote text-muted-foreground">在飞书开放平台打开，已经勾好要开的权限</span></li>
          <li className="flex flex-wrap items-center gap-2"><span className="text-muted-foreground">2.</span>
            {app ? <a className="inline-flex items-center gap-1 text-blue hover:underline" href={app.publish_url} target="_blank" rel="noreferrer noopener" data-publish-link>开通后发布新版本<ExternalLinkIcon className="size-3" /></a> : <span>开通后发布新版本</span>}
            <span className="type-footnote text-muted-foreground">权限要发布后才生效；敏感权限要管理员审批</span></li>
          <li className="flex flex-wrap items-center gap-2"><span className="text-muted-foreground">3.</span>
            <Button size="sm" variant="outline" disabled={busy} onClick={() => void check()} data-recheck>{busy ? <Spinner /> : <RefreshCwIcon />}我已开通，重新检查</Button></li>
        </ol>
      </> : <div className="flex flex-wrap items-center gap-2">
        <span className="type-footnote text-muted-foreground">能查到的都已开通{app?.checked_at ? ` · ${tm(app.checked_at)} 检查过` : ""}。Steve 工作时如果被飞书拒绝，会记在这里。</span>
        <Button size="sm" variant="ghost" className="-ml-1" disabled={busy} onClick={() => void check()} data-recheck>{busy ? <Spinner /> : <RefreshCwIcon />}重新检查</Button></div>}
    </div>
  )
}

/** your permissions: re-authorize with the missing scopes; the console link when the app hasn't enabled them */
function UserPermissions({ c, app, appConn }: { c: IConnection; app?: FeishuAppDetail; appConn?: IConnection }) {
  const rows = missingOf(c), d = userOf(c), via = d?.app_connection ?? appConn?.id
  const need = rows.length > 0 || c.status === "reauth"
  if (!need) return <div className="px-1 type-footnote text-muted-foreground" data-permissions-block="you" data-missing-count={0}>你的授权齐全。以后 Steve 工作时如果被飞书拒绝，会记在这里。</div>
  return (
    <div className="flex flex-col gap-2 rounded-xl bg-muted/40 px-3.5 py-3" data-permissions-block="you" data-missing-count={rows.length}>
      <div className="type-callout font-medium">{rows.length ? `你的授权缺 ${rows.length} 项` : "授权过期了"}</div>
      {rows.length > 0 && <MissingList rows={rows} />}
      <div className="flex flex-wrap items-center gap-2">
        <SelfButtons app={via ?? ""} disabled={!via} label="重新授权" scopes={rows.map((x) => x.scope)} reauthOf={c.id} />
        <span className="type-footnote text-muted-foreground">在飞书里再同意一次，{rows.length ? "这次会带上缺的几项" : "Steve 拿到新的令牌"}</span>
      </div>
      {rows.length > 0 && app && <div className="type-footnote text-muted-foreground">飞书说应用没开通这些权限？先 <a className="text-blue hover:underline" href={feishuGrantUrl(app.domain, app.app_id, rows.map((x) => x.scope), false)} target="_blank" rel="noreferrer noopener" data-user-grant-link>在开发者后台开通（用户身份）</a>，发布新版本后再重新授权。</div>}
    </div>
  )
}

/** one speaker per group: the bot where it is, you elsewhere (domain-model.md §1.4) */
function VoiceRule({ bot, me }: { bot: ChannelAccount | null; me: ChannelAccount | null }) {
  const groups = (a: ChannelAccount | null) => (a?.spaces ?? []).filter((s) => s.status === "active" && s.kind !== "dm")
  // one space per chat: a group both identities see is the same space in both lists
  const mine = groups(me), shared: Space[] = groups(bot).filter((s) => mine.some((m) => m.id === s.id))
  return (
    <div className="flex gap-2.5 border-t bg-muted/30 px-4 py-3" data-voice-rule data-shared-groups={shared.length}>
      <InfoIcon className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
      <div className="min-w-0 flex-1 type-callout">
        <div className="font-medium">两个身份不会抢着回答</div>
        <div className="text-muted-foreground">一个群里只有一个身份替 Steve 说话。机器人在的群默认由机器人回答，代你处理在那里只观察，不起草、不回复；机器人不在的群，才由代你处理起草或回复。哪个群由谁说话，可以在“渠道”里按群改。</div>
        {shared.length > 0 && <div className="mt-1.5 flex flex-wrap items-center gap-x-2 gap-y-1 type-footnote text-muted-foreground"><span>两个身份都在的群：</span>
          {shared.slice(0, 6).map((s) => <a key={s.id} className="text-blue hover:underline" href={"#/channels/space/" + s.id} data-shared-group>{s.name}</a>)}
          {shared.length > 6 && <span>等 {shared.length} 个</span>}</div>}
      </div>
    </div>
  )
}

/** what a technical reader wants, out of the way */
function Tech({ inst, p }: { inst: IInstallation; p: IProvider }) {
  const feishu = isFeishu(p.id)
  return (
    <Collapsible className="min-w-0 flex-1">
      <CollapsibleTrigger className="rounded type-footnote text-muted-foreground hover:text-foreground hover:underline" data-tech-toggle>技术细节</CollapsibleTrigger>
      <CollapsibleContent className="mt-2 flex flex-col gap-2 type-footnote text-muted-foreground" data-tech>
        {inst.identities.map(({ kind, connection: c }) => { const a = appOf(c), u = userOf(c); return (
          <dl key={c.id} className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5">
            <dt>{kind === "steve" ? "机器人连接" : "你的连接"}</dt><dd className="break-all font-mono text-[12px]">{c.id} · {c.adapter}</dd>
            {a && <><dt>App ID</dt><dd className="break-all font-mono text-[12px]">{a.app_id}</dd></>}
            {feishu && <><dt>令牌</dt><dd>{kind === "steve" ? "tenant_access_token（应用身份，约 2 小时自动换新）" : "user_access_token + refresh_token（你的身份，自动刷新）"}</dd></>}
            {u && <><dt>申请的用户权限</dt><dd className="break-all font-mono text-[12px]">{u.requested.join(" ")}</dd></>}
            <dt>上次正常</dt><dd>{c.health.last_ok ? tm(c.health.last_ok) : "—"}</dd>
            {c.health.last_error && <><dt>最近的错误</dt><dd className="break-all">{c.health.last_error}</dd></>}
            {a?.grant && <><dt>申请链接</dt><dd className="break-all font-mono text-[12px]">{a.grant.url}（{a.grant.from === "feishu" ? "飞书在报错里给的" : "Steve 按缺的权限拼的"}）</dd></>}
          </dl>) })}
      </CollapsibleContent>
    </Collapsible>
  )
}
