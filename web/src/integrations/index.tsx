/* 接入 (integrations): every outside service Steve can connect — 飞书 / Lark, GitHub, mail, and the ones still planned.
 * Installation → identity → capability (domain-model.md §1.3): under each provider its installations (a Feishu app, a
 * GitHub App, a mailbox), each with the identities Steve works through — itself, and on your behalf — and for each what
 * it can do: 已开启 / 缺权限 / 已关闭 / 规划中. Nothing here claims a capability works without a connection that is up.
 * Routes: #/integrations, #/integrations/:provider. Data: GET /connections/integrations, the 渠道 accounts for defaults. */
import { useState } from "react"
import { CalendarIcon, ChevronRightIcon, GitPullRequestIcon, HashIcon, MailIcon, MessagesSquareIcon, PlugIcon, PlusIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { go, Page, Group, Row, SectionLabel, Meta, Empty, StatusDot } from "@/ui"
import { Unwired } from "@/unwired"
import { useLoad } from "@/accounts/load"
import { CONN_STATUS } from "@/api/accounts"
import { ConnectDialog } from "@/accounts/dialogs"
import { integrationsApi, areaLabel, asAdapter, installationsOf, type IAdapter, type IInstallation, type IProvider, type Integrations } from "@/api/integrations"
import { channelsApi } from "@/api/channels"
import { cleanupApi, type Leftover } from "@/api/cleanup"
import { FeishuGuide } from "@/integrations/feishu"
import { InstallationCard } from "@/integrations/installation"
import { CleanupDialog } from "@/integrations/disconnect"

const ICON: Record<string, any> = { feishu: MessagesSquareIcon, github: GitPullRequestIcon, mail: MailIcon, slack: HashIcon, gcal: CalendarIcon }
export const ProviderIcon = ({ id, size = 32 }: { id: string; size?: number }) => {
  const I = ICON[id] ?? PlugIcon
  return <span className="flex shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground" style={{ width: size, height: size }} aria-hidden><I className="size-4" strokeWidth={1.5} /></span>
}
const Note = ({ children }: { children: React.ReactNode }) => <div className="mt-2 px-1 type-footnote text-muted-foreground">{children}</div>
const Hint = ({ children, ...rest }: { children: React.ReactNode; [k: string]: any }) => <div className="rounded-xl border border-dashed px-4 py-3.5 type-callout text-muted-foreground" {...rest}>{children}</div>
const join = (xs: string[]) => xs.join("、")
/** #/integrations and #/integrations/:provider */
export function IntegrationsRoute({ id }: { id?: string }) {
  const L = useLoad(() => integrationsApi.all(), [], ["connection", "account"])
  if (L.s === "unwired") return <Unwired id="integrations" />
  const back: [string, string] | undefined = id ? ["#/integrations", "接入"] : undefined
  if (L.s === "loading") return <Page title="接入" back={back}><div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div></Page>
  if (L.s === "error") return <Page title="接入" back={back}><Empty title={L.err} /></Page>
  if (!id) return <Overview x={L.data} />
  const p = L.data.providers.find((x) => x.id === id)
  return p ? <ProviderView key={p.id} p={p} x={L.data} reload={() => void L.reload()} /> : <Page title="接入" back={back}><Empty title="没有这个服务" sub="Steve 还不认识它，回到接入页看看能接入哪些。" /></Page>
}

/* ═════════ overview ═════════ */
function Overview({ x }: { x: Integrations }) {
  const live = x.providers.filter((p) => p.status === "supported"), soon = x.providers.filter((p) => p.status === "planned")
  const n = live.reduce((k, p) => k + p.connections.length, 0)
  return (
    <Page title="接入" sub="Steve 能连接的外部服务。渠道只是其中一种：还有文档、日历、任务、邮件、代码仓库和通讯录。每个接入里 Steve 以谁的身份工作、能做什么，都列在下面" max={760}>
      <div className="flex flex-col gap-8" data-integrations data-connections={n}>
        {n === 0 && <div className="rounded-xl border px-4 py-3.5" data-none-connected><div className="type-headline">还没有接入任何服务</div>
          <div className="mt-0.5 type-callout text-muted-foreground">下面的服务 Steve 已经支持，但还没有接入，所以现在一项能力都没有开启。点一个服务，按步骤接入。</div></div>}
        <section><SectionLabel>可以接入 · {live.length}</SectionLabel>
          <div className="flex flex-col gap-6">{live.map((p) => <ProviderBlock key={p.id} p={p} x={x} />)}</div></section>
        {soon.length > 0 && <section><SectionLabel>即将支持 · {soon.length}</SectionLabel>
          <Group>{soon.map((p) => <Row key={p.id} icon={<ProviderIcon id={p.id} />} title={p.name} sub={"规划中：" + join(p.areas.map((a) => areaLabel(a.area)))} onClick={() => go("#/integrations/" + p.id)} data-provider-row={p.id}>
            <span className="type-footnote text-muted-foreground">即将支持</span><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Group>
          <Note>这些服务在计划里，Steve 还不能接入。</Note></section>}
        <Note>凭据和令牌加密存在保险箱里。权限、重新授权和断开在每个服务的页面里；群和私聊怎么处理在“渠道”。</Note>
      </div>
    </Page>
  )
}

/** one provider: its name and state, one line per installation */
function ProviderBlock({ p, x }: { p: IProvider; x: Integrations }) {
  if (p.connections.length) return <InstallBlock p={p} insts={installationsOf(x, p.id)} />
  return <ProviderBlockOld p={p} />
}

/** “Steve 机器人 · 代你处理 · 缺 N 项权限”: who Steve works as in this installation, and what to fix */
export function installationLine(i: IInstallation, p: IProvider) {
  const feishu = p.id === "feishu", has = (k: string) => i.identities.some((y) => y.kind === k)
  const offers = (k: string) => p.adapters.some((a) => (a.identity === "bot" || a.identity === "app" ? "steve" : "you") === k)
  const parts: string[] = []
  if (has("steve")) parts.push(feishu ? "Steve 机器人" : "Steve 自己的身份")
  else if (offers("steve") && feishu) parts.push("机器人未开启")
  if (has("you")) parts.push("代你处理")
  else if (offers("you") && feishu) parts.push("代你处理未开启")
  parts.push(i.missing ? `缺 ${i.missing} 项权限` : i.status === "ok" ? "权限齐全" : CONN_STATUS[i.status]?.[0] ?? i.status)
  return parts.join(" · ")
}

function InstallBlock({ p, insts }: { p: IProvider; insts: IInstallation[] }) {
  return (
    <div data-provider={p.id} data-provider-state="connected">
      <Group>
        <Row icon={<ProviderIcon id={p.id} />} onClick={() => go("#/integrations/" + p.id)} data-provider-row={p.id}
          title={<span className="inline-flex flex-wrap items-center gap-x-2 gap-y-0.5"><span className="type-headline">{p.name}</span>
            <span className="type-footnote text-muted-foreground">已接入 {insts.length} 个{p.id === "feishu" ? "应用" : ""}</span></span>}>
          <ChevronRightIcon className="size-4 text-tertiary" /></Row>
        {insts.map((i) => { const [, dot] = CONN_STATUS[i.status] ?? [i.status, "idle"]; return (
          <Row key={i.id} className="pl-12" onClick={() => go("#/integrations/" + p.id)} data-installation-row={i.id} data-installation-missing={i.missing}
            title={<span className="inline-flex items-center gap-1.5">{i.name}{i.status !== "ok" && <StatusDot s={dot} />}</span>}
            sub={<span className={i.missing ? "text-attention" : undefined} data-installation-line>{installationLine(i, p)}</span>}>
            <ChevronRightIcon className="size-4 text-tertiary" /></Row>) })}
      </Group>
    </div>
  )
}

/** a provider without connections: what it supports */
function ProviderBlockOld({ p }: { p: IProvider }) {
  const sup = p.areas.filter((a) => a.state === "supported").map((a) => areaLabel(a.area)), plan = p.areas.filter((a) => a.state === "planned").map((a) => areaLabel(a.area))
  return (
    <div data-provider={p.id} data-provider-state={p.connections.length ? "connected" : "none"}>
      <Group>
        <Row icon={<ProviderIcon id={p.id} />} onClick={() => go("#/integrations/" + p.id)} data-provider-row={p.id}
          title={<span className="inline-flex flex-wrap items-center gap-x-2 gap-y-0.5"><span className="type-headline">{p.name}</span>
            <span className="type-footnote text-muted-foreground">{p.connections.length ? `已接入 ${p.connections.length} 个` : "还没有接入"}</span></span>}
          sub={[sup.length ? "支持：" + join(sup) : "", plan.length ? "规划中：" + join(plan) : ""].filter(Boolean).join(" · ")}>
          {!p.connections.length && <span className="hidden type-callout text-blue sm:inline">接入</span>}<ChevronRightIcon className="size-4 text-tertiary" /></Row>
      </Group>
    </div>
  )
}

/* ═════════ one provider ═════════ */
function ProviderView({ p, x, reload }: { p: IProvider; x: Integrations; reload: () => void }) {
  const [dlg, setDlg] = useState<IAdapter | null>(null)
  const insts = installationsOf(x, p.id)
  // the identities' defaults (rules, /bot, persona) and their groups; absent on a Hub without 渠道
  const C = useLoad(() => (p.connections.some((c) => c.capabilities.some((a) => a.area === "channel" && a.state !== "planned")) ? channelsApi.accounts() : Promise.resolve([])), [p.id, p.connections.length], ["connection", "space", "channel"])
  const chans = C.s === "ok" ? C.data : null
  const bots = p.connections.filter((c) => c.adapter === "feishu_app").map((c) => {
    const raw = c.detail && "domain" in c.detail ? c.detail.domain : undefined
    const domain: "feishu" | "lark" | undefined = raw === "lark" ? "lark" : raw === "feishu" ? "feishu" : undefined
    return { id: c.id, name: c.name || c.handle, domain }
  })
  const planned = p.status === "planned"
  return (
    <Page title={p.name} back={["#/integrations", "接入"]} max={720}
      sub={<Meta><span>{planned ? "即将支持" : insts.length ? `已接入 ${insts.length} 个${p.id === "feishu" ? "应用" : ""}` : "还没有接入"}</span>{p.adapters.length > 0 && p.id !== "feishu" && <span>{p.adapters.map((a) => a.name).join(" / ")}</span>}</Meta>}>
      {dlg && <ConnectDialog adapter={asAdapter(dlg, p.id)} onClose={() => setDlg(null)} done={() => go("#/integrations/" + p.id)} />}
      <div className="flex flex-col gap-8" data-provider-page={p.id}>
        {planned ? <Hint data-planned>Steve 还不能接入{p.name}。它在计划里：{join(p.planned.map((w) => w.name))}。做好之前这里不会有连接，也不会有任何能力开启。</Hint>
          : insts.length ? <section><SectionLabel>已接入 · {insts.length}</SectionLabel>
            <div className="flex flex-col gap-6">{insts.map((i) => <InstallationCard key={i.id} inst={i} p={p} chans={chans} onChange={reload} />)}</div></section>
          : <section><SectionLabel>已接入 · 0</SectionLabel><Hint data-not-connected>还没有接入{p.name}。选择适合你的接入方式，完成后即可开启下方能力。</Hint></section>}
        {!planned && <Leftovers p={p} />}
        {p.id === "feishu" ? <FeishuGuide adapters={p.adapters} bots={bots} selfDone={p.connections.some((c) => c.adapter === "feishu_user")} callbackUrls={x.callback_urls} />
          : !planned && <WaysIn p={p} onConnect={setDlg} />}
        {!insts.length && <section><SectionLabel>能做什么</SectionLabel>
          <Group>{[...p.areas].sort((x, y) => +(x.state === "planned") - +(y.state === "planned")).map((a) => <Row key={a.area} title={areaLabel(a.area)} sub={areaNote(p, a.area)} data-area={a.area} data-area-state={a.state}>
            <span className="inline-flex items-center gap-1.5 type-footnote text-muted-foreground"><StatusDot s={a.state === "supported" ? (p.connections.length ? "done" : "idle") : "drop"} />{a.state === "planned" ? "规划中" : p.connections.length ? "已支持" : "已支持 · 未接入"}</span></Row>)}</Group>
          {!planned && !p.connections.length && <Note>“已支持”指 Steve 能做，接入之后才会开启。</Note>}</section>}
        {p.planned.length > 0 && !planned && <section><SectionLabel>即将支持</SectionLabel>
          <Group>{p.planned.map((w) => <Row key={w.name} title={w.name} sub={[join(w.areas.map(areaLabel)), w.note].filter(Boolean).join(" · ")}><span className="type-footnote text-muted-foreground">规划中</span></Row>)}</Group></section>}
      </div>
    </Page>
  )
}

/** what an area covers, from the adapter that offers it */
function areaNote(p: IProvider, area: string) {
  for (const a of p.adapters) { const c = a.capabilities.find((x) => x.area === area); if (c?.note && c.note !== areaLabel(area)) return c.note }
  return undefined
}

const WAY_HINT: Record<string, string> = {
  github_app: "推荐。在 GitHub 页面一键创建 GitHub App，装到你选的仓库；事件实时推送",
  github_pat: "细粒度个人令牌：选好仓库，给 Contents、Pull requests、Issues 读写，Checks、Actions 读",
  imap: "Gmail、Outlook、企业邮箱都行：服务器地址加应用专用密码",
}
/** the generic ways in: a credential form, OAuth (with the device code when offered), or a module's own flow */
function WaysIn({ p, onConnect }: { p: IProvider; onConnect: (a: IAdapter) => void }) {
  return (
    <section><SectionLabel>怎么接入</SectionLabel>
      <Group>{p.adapters.map((a) => (
        <Row key={a.id} title={a.name} sub={WAY_HINT[a.id] ?? (a.auth === "oauth" ? "在浏览器里授权，Steve 只保存访问令牌" : "凭据加密存进保险箱")} data-way={a.id}>
          {a.auth === "managed" ? <Button size="sm" variant="outline" onClick={() => go("#/github")} data-way-go={a.id}>去创建</Button>
            : <Button size="sm" variant="outline" onClick={() => onConnect(a)} data-way-go={a.id}><PlusIcon />接入</Button>}</Row>))}</Group>
    </section>
  )
}

/** identities disconnected earlier whose groups and messages are still here: clean them up */
function Leftovers({ p }: { p: IProvider }) {
  const L = useLoad(() => cleanupApi.leftovers(), [], ["connection", "space"])
  const [pick, setPick] = useState<Leftover | null>(null)
  if (L.s !== "ok") return null
  const systems = new Set([p.id, ...p.adapters.map((a) => a.id), p.id + "_app"])
  const xs = L.data.filter((l) => systems.has(l.system_id))
  if (!xs.length) return null
  return (
    <section data-leftovers><SectionLabel>已断开 · 还留着记录</SectionLabel>
      {pick && <CleanupDialog target={{ account_id: pick.account_id }} name={pick.name} onClose={() => setPick(null)} onDone={() => { setPick(null); void L.reload() }} />}
      <Group>{xs.map((l) => <Row key={l.account_id} title={l.name} sub={`${l.spaces} 个群和私聊、${l.messages} 条消息，标为“已离开”`} data-leftover={l.account_id}>
        <Button size="sm" variant="outline" onClick={() => setPick(l)} data-leftover-clean>清理记录</Button></Row>)}</Group>
      <Note>断开时选了“保留全部”的身份，记录还能在渠道里搜到。不需要了可以清理。</Note>
    </section>
  )
}
