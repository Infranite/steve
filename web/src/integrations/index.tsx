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
import { t as tr, useT } from "@/i18n"
import { useLoad } from "@/accounts/load"
import { CONN_STATUS } from "@/api/accounts"
import { ConnectDialog } from "@/accounts/dialogs"
import { botAdapterIds } from "@/integrations/sides"
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
const join = (xs: string[]) => xs.join(tr("integrations:list.sep"))
/** #/integrations and #/integrations/:provider */
export function IntegrationsRoute({ id }: { id?: string }) {
  const t = useT()
  const L = useLoad(() => integrationsApi.all(), [], ["connection", "account"])
  if (L.s === "unwired") return <Unwired id="integrations" />
  const back: [string, string] | undefined = id ? ["#/integrations", t("integrations:page.title")] : undefined
  if (L.s === "loading") return <Page title={t("integrations:page.title")} back={back}><div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div></Page>
  if (L.s === "error") return <Page title={t("integrations:page.title")} back={back}><Empty title={L.err} /></Page>
  if (!id) return <Overview x={L.data} />
  const p = L.data.providers.find((x) => x.id === id)
  return p ? <ProviderView key={p.id} p={p} x={L.data} reload={() => void L.reload()} /> : <Page title={t("integrations:page.title")} back={back}><Empty title={t("integrations:page.notFound")} sub={t("integrations:page.notFoundSub")} /></Page>
}

/* ═════════ overview ═════════ */
function Overview({ x }: { x: Integrations }) {
  const t = useT()
  const live = x.providers.filter((p) => p.status === "supported"), soon = x.providers.filter((p) => p.status === "planned")
  const n = live.reduce((k, p) => k + p.connections.length, 0)
  return (
    <Page title={t("integrations:page.title")} sub={t("integrations:page.overviewSub")} max={760}>
      <div className="flex flex-col gap-8" data-integrations data-connections={n}>
        {n === 0 && <div className="rounded-xl border px-4 py-3.5" data-none-connected><div className="type-headline">{t("integrations:page.noneTitle")}</div>
          <div className="mt-0.5 type-callout text-muted-foreground">{t("integrations:page.noneSub")}</div></div>}
        <section><SectionLabel>{t("integrations:page.available", { n: live.length })}</SectionLabel>
          <div className="flex flex-col gap-6">{live.map((p) => <ProviderBlock key={p.id} p={p} x={x} />)}</div></section>
        {soon.length > 0 && <section><SectionLabel>{t("integrations:page.comingSoonN", { n: soon.length })}</SectionLabel>
          <Group>{soon.map((p) => <Row key={p.id} icon={<ProviderIcon id={p.id} />} title={p.name} sub={t("integrations:page.plannedList", { list: join(p.areas.map((a) => areaLabel(a.area))) })} onClick={() => go("#/integrations/" + p.id)} data-provider-row={p.id}>
            <span className="type-footnote text-muted-foreground">{t("integrations:page.comingSoon")}</span><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Group>
          <Note>{t("integrations:page.soonNote")}</Note></section>}
        <Note>{t("integrations:page.footNote")}</Note>
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
  if (has("steve")) parts.push(feishu ? tr("integrations:line.steveBot") : tr("integrations:line.steveSelf"))
  else if (offers("steve") && feishu) parts.push(tr("integrations:line.botOff"))
  if (has("you")) parts.push(tr("integrations:line.you"))
  else if (offers("you") && feishu) parts.push(tr("integrations:line.youOff"))
  parts.push(i.missing ? tr("integrations:line.missing", { count: i.missing }) : i.status === "ok" ? tr("integrations:line.allGranted") : CONN_STATUS[i.status]?.[0] ?? i.status)
  return parts.join(" · ")
}

function InstallBlock({ p, insts }: { p: IProvider; insts: IInstallation[] }) {
  const t = useT()
  return (
    <div data-provider={p.id} data-provider-state="connected">
      <Group>
        <Row icon={<ProviderIcon id={p.id} />} onClick={() => go("#/integrations/" + p.id)} data-provider-row={p.id}
          title={<span className="inline-flex flex-wrap items-center gap-x-2 gap-y-0.5"><span className="type-headline">{p.name}</span>
            <span className="type-footnote text-muted-foreground">{t(p.id === "feishu" ? "integrations:page.connectedApps" : "integrations:page.connectedN", { count: insts.length })}</span></span>}>
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
  const t = useT()
  const sup = p.areas.filter((a) => a.state === "supported").map((a) => areaLabel(a.area)), plan = p.areas.filter((a) => a.state === "planned").map((a) => areaLabel(a.area))
  return (
    <div data-provider={p.id} data-provider-state={p.connections.length ? "connected" : "none"}>
      <Group>
        <Row icon={<ProviderIcon id={p.id} />} onClick={() => go("#/integrations/" + p.id)} data-provider-row={p.id}
          title={<span className="inline-flex flex-wrap items-center gap-x-2 gap-y-0.5"><span className="type-headline">{p.name}</span>
            <span className="type-footnote text-muted-foreground">{p.connections.length ? t("integrations:page.connectedN", { count: p.connections.length }) : t("integrations:page.notConnected")}</span></span>}
          sub={[sup.length ? t("integrations:page.supportedList", { list: join(sup) }) : "", plan.length ? t("integrations:page.plannedList", { list: join(plan) }) : ""].filter(Boolean).join(" · ")}>
          {!p.connections.length && <span className="hidden type-callout text-blue sm:inline">{t("integrations:page.connect")}</span>}<ChevronRightIcon className="size-4 text-tertiary" /></Row>
      </Group>
    </div>
  )
}

/* ═════════ one provider ═════════ */
function ProviderView({ p, x, reload }: { p: IProvider; x: Integrations; reload: () => void }) {
  const t = useT()
  const [dlg, setDlg] = useState<IAdapter | null>(null)
  const insts = installationsOf(x, p.id)
  // the identities' defaults (rules, /bot, persona) and their groups; absent on a Hub without 渠道
  const C = useLoad(() => (p.connections.some((c) => c.capabilities.some((a) => a.area === "channel" && a.state !== "planned")) ? channelsApi.accounts() : Promise.resolve([])), [p.id, p.connections.length], ["connection", "space", "channel"])
  const chans = C.s === "ok" ? C.data : null
  const botIds = botAdapterIds(p.adapters), selfIds = p.adapters.filter((a) => a.identity === "self").map((a) => a.id)
  const bots = p.connections.filter((c) => botIds.includes(c.adapter)).map((c) => {
    const raw = c.detail && "domain" in c.detail ? c.detail.domain : undefined
    const domain: "feishu" | "lark" | undefined = raw === "lark" ? "lark" : raw === "feishu" ? "feishu" : undefined
    return { id: c.id, name: c.name || c.handle, domain }
  })
  const planned = p.status === "planned"
  return (
    <Page title={p.name} back={["#/integrations", t("integrations:page.title")]} max={720}
      sub={<Meta><span>{planned ? t("integrations:page.comingSoon") : insts.length ? t(p.id === "feishu" ? "integrations:page.connectedApps" : "integrations:page.connectedN", { count: insts.length }) : t("integrations:page.notConnected")}</span>{p.adapters.length > 0 && p.id !== "feishu" && <span>{p.adapters.map((a) => a.name).join(" / ")}</span>}</Meta>}>
      {dlg && <ConnectDialog adapter={asAdapter(dlg, p.id)} onClose={() => setDlg(null)} done={() => go("#/integrations/" + p.id)} />}
      <div className="flex flex-col gap-8" data-provider-page={p.id}>
        {planned ? <Hint data-planned>{t("integrations:page.plannedHint", { name: p.name, list: join(p.planned.map((w) => w.name)) })}</Hint>
          : insts.length ? <section><SectionLabel>{t("integrations:page.connectedSection", { n: insts.length })}</SectionLabel>
            <div className="flex flex-col gap-6">{insts.map((i) => <InstallationCard key={i.id} inst={i} p={p} chans={chans} onChange={reload} />)}</div></section>
          : <section><SectionLabel>{t("integrations:page.connectedSection", { n: 0 })}</SectionLabel><Hint data-not-connected>{t("integrations:page.notConnectedHint", { name: p.name })}</Hint></section>}
        {!planned && <Leftovers p={p} />}
        {p.id === "feishu" ? <FeishuGuide adapters={p.adapters} bots={bots} selfDone={p.connections.some((c) => selfIds.includes(c.adapter))} callbackUrls={x.callback_urls} />
          : !planned && <WaysIn p={p} onConnect={setDlg} />}
        {!insts.length && <section><SectionLabel>{t("integrations:page.whatItDoes")}</SectionLabel>
          <Group>{[...p.areas].sort((x, y) => +(x.state === "planned") - +(y.state === "planned")).map((a) => <Row key={a.area} title={areaLabel(a.area)} sub={areaNote(p, a.area)} data-area={a.area} data-area-state={a.state}>
            <span className="inline-flex items-center gap-1.5 type-footnote text-muted-foreground"><StatusDot s={a.state === "supported" ? (p.connections.length ? "done" : "idle") : "drop"} />{a.state === "planned" ? t("integrations:page.planned") : p.connections.length ? t("integrations:page.supported") : t("integrations:page.supportedNotConnected")}</span></Row>)}</Group>
          {!planned && !p.connections.length && <Note>{t("integrations:page.supportedNote")}</Note>}</section>}
        {p.planned.length > 0 && !planned && <section><SectionLabel>{t("integrations:page.comingSoon")}</SectionLabel>
          <Group>{p.planned.map((w) => <Row key={w.name} title={w.name} sub={[join(w.areas.map(areaLabel)), w.note].filter(Boolean).join(" · ")}><span className="type-footnote text-muted-foreground">{t("integrations:page.planned")}</span></Row>)}</Group></section>}
      </div>
    </Page>
  )
}

/** what an area covers, from the adapter that offers it */
function areaNote(p: IProvider, area: string) {
  for (const a of p.adapters) { const c = a.capabilities.find((x) => x.area === area); if (c?.note && c.note !== areaLabel(area)) return c.note }
  return undefined
}

/** the hint under a way in, by adapter id (the id is the Hub's; the text is ours) */
const WAY_HINT: Record<string, "githubApp" | "githubPat" | "imap"> = { github_app: "githubApp", github_pat: "githubPat", imap: "imap" }
/** the generic ways in: a credential form, OAuth (with the device code when offered), or a module's own flow */
function WaysIn({ p, onConnect }: { p: IProvider; onConnect: (a: IAdapter) => void }) {
  const t = useT()
  return (
    <section><SectionLabel>{t("integrations:page.waysIn")}</SectionLabel>
      <Group>{p.adapters.map((a) => (
        <Row key={a.id} title={a.name} sub={WAY_HINT[a.id] ? t(`integrations:way.${WAY_HINT[a.id]}`) : a.auth === "oauth" ? t("integrations:page.wayOAuth") : t("integrations:page.wayCredential")} data-way={a.id}>
          {a.auth === "managed" ? <Button size="sm" variant="outline" onClick={() => go("#/github")} data-way-go={a.id}>{t("integrations:page.goCreate")}</Button>
            : <Button size="sm" variant="outline" onClick={() => onConnect(a)} data-way-go={a.id}><PlusIcon />{t("integrations:page.connect")}</Button>}</Row>))}</Group>
    </section>
  )
}

/** identities disconnected earlier whose groups and messages are still here: clean them up */
function Leftovers({ p }: { p: IProvider }) {
  const t = useT()
  const L = useLoad(() => cleanupApi.leftovers(), [], ["connection", "space"])
  const [pick, setPick] = useState<Leftover | null>(null)
  if (L.s !== "ok") return null
  const systems = new Set([p.id, ...p.adapters.map((a) => a.id), p.id + "_app"])
  const xs = L.data.filter((l) => systems.has(l.system_id))
  if (!xs.length) return null
  return (
    <section data-leftovers><SectionLabel>{t("integrations:page.leftovers")}</SectionLabel>
      {pick && <CleanupDialog target={{ account_id: pick.account_id }} name={pick.name} onClose={() => setPick(null)} onDone={() => { setPick(null); void L.reload() }} />}
      <Group>{xs.map((l) => <Row key={l.account_id} title={l.name} sub={t("integrations:page.leftoverSub", { chats: t("integrations:page.leftoverChats", { count: l.spaces }), messages: t("integrations:page.leftoverMessages", { count: l.messages }) })} data-leftover={l.account_id}>
        <Button size="sm" variant="outline" onClick={() => setPick(l)} data-leftover-clean>{t("integrations:page.cleanup")}</Button></Row>)}</Group>
      <Note>{t("integrations:page.leftoverNote")}</Note>
    </section>
  )
}
