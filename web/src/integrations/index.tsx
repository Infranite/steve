/* 接入 (integrations): every outside service Steve can connect — 飞书 / Lark, GitHub, mail.
 * Installation → identity → capability (domain-model.md §1.3): under each provider its installations (a Feishu app, a
 * GitHub App, a mailbox), each with the identities Steve works through — itself, and on your behalf — and for each what
 * it can do: 已开启 / 缺权限 / 已关闭 / 规划中. Nothing here claims a capability works without a connection that is up.
 * Routes: #/integrations, #/integrations/:provider. Data: GET /connections/integrations, the 渠道 accounts for defaults. */
import { useState } from "react"
import { CalendarIcon, ContactRoundIcon, FileTextIcon, GitPullRequestIcon, HashIcon, ListChecksIcon, MailIcon, MessagesSquareIcon, MicIcon, PlugIcon, PlusIcon, SendIcon, SheetIcon, StampIcon, WebhookIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { go, Page, Group, Row, SectionLabel, Meta, Empty, StatusDot } from "@/ui"
import { AppDirectory, type App, type AppCategory, type AppDirectoryLabels } from "@/components/affect/app-directory-clean"
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

const ICON: Record<string, any> = { ext_ws: WebhookIcon, feishu: MessagesSquareIcon, github: GitPullRequestIcon, mail: MailIcon, slack: HashIcon, gcal: CalendarIcon }
/** the provider's mark, for the tiles of the directory and the installation cards (they draw the frame) */
export const ProviderGlyph = ({ id }: { id: string }) => { const I = ICON[id] ?? PlugIcon; return <I className="size-4 text-muted-foreground" strokeWidth={1.5} aria-hidden /> }
const Note = ({ children }: { children: React.ReactNode }) => <div className="mt-3 type-footnote text-muted-foreground">{children}</div>
const Hint = ({ children, ...rest }: { children: React.ReactNode; [k: string]: any }) => <div className="rounded-xl bg-muted/60 px-4 py-3.5 type-callout text-muted-foreground" {...rest}>{children}</div>
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
/** each capability area's icon in the directory's categories */
const AREA_ICON: Record<string, any> = { channel: MessagesSquareIcon, send_as_you: SendIcon, docs: FileTextIcon, sheets: SheetIcon, calendar: CalendarIcon, tasks: ListChecksIcon, approval: StampIcon, minutes: MicIcon, mail: MailIcon, repo: GitPullRequestIcon, directory: ContactRoundIcon }
const areaIcon = (area: string) => { const I = AREA_ICON[area] ?? PlugIcon; return <I className="size-4" strokeWidth={1.5} aria-hidden /> }

/** every provider as a card of the directory, its categories the areas it supports today (planned ones are only named) */
function Overview({ x }: { x: Integrations }) {
  const t = useT()
  const live = x.providers
  const n = live.reduce((k, p) => k + p.connections.length, 0)
  const supported = (p: IProvider) => p.areas.filter((a) => a.state === "supported").map((a) => a.area)
  const order = x.areas.map((a) => a.id)
  const areas = [...new Set(live.flatMap(supported))].sort((a, b) => order.indexOf(a) - order.indexOf(b))
  const categories: AppCategory[] = areas.map((a) => ({ id: a, label: areaLabel(a), icon: areaIcon(a) }))
  const apps: App[] = live.map((p) => {
    const insts = installationsOf(x, p.id), on = p.connections.length > 0
    const sup = p.areas.filter((a) => a.state === "supported").map((a) => areaLabel(a.area)), plan = p.areas.filter((a) => a.state === "planned").map((a) => areaLabel(a.area))
    const attention = insts.some((i) => i.missing > 0 || i.status !== "ok")
    return {
      id: p.id, name: p.name, connected: on, attention, categories: supported(p), logo: <ProviderGlyph id={p.id} />,
      vendor: on ? (attention ? t("integrations:directory.attention") : t(p.id === "feishu" ? "integrations:page.connectedApps" : "integrations:page.connectedN", { count: insts.length })) : t("integrations:page.notConnected"),
      keywords: [...sup, ...plan, ...p.adapters.map((a) => a.name), ...insts.map((i) => i.name)].join(" "),
      description: on
        ? <span className="flex flex-col">{insts.map((i) => {
            const name = <><span className="text-foreground">{i.name}</span>{i.status !== "ok" && <> <StatusDot s={(CONN_STATUS[i.status] ?? [i.status, "idle"])[1]} /></>}</>
            const line = <span className={i.missing ? "text-attention" : undefined} data-installation-line>{installationLine(i, p)}</span>
            // one installation: its name, then its line; several: one line each
            return insts.length === 1
              ? <span key={i.id} className="flex flex-col" data-installation-row={i.id} data-installation-missing={i.missing}><span className="truncate">{name}</span><span className="truncate">{line}</span></span>
              : <span key={i.id} className="truncate" data-installation-row={i.id} data-installation-missing={i.missing}>{name}<span className="text-tertiary"> · </span>{line}</span>
          })}</span>
        // what it supports today, as plain text: it wraps, the card clamps it at two lines and its tooltip has it all;
        // the planned areas are on the provider's page (and still found by the search)
        : sup.length ? t("integrations:page.supportedList", { list: join(sup) }) : "",
      attrs: { "data-provider": p.id, "data-provider-state": on ? "connected" : "none", "data-provider-row": p.id },
    }
  })
  const labels: AppDirectoryLabels = {
    connected: (count) => t("integrations:directory.connected", { count }), search: t("integrations:directory.search"), filter: t("integrations:directory.filter"),
    all: t("integrations:directory.all"), onlyConnected: t("integrations:directory.onlyConnected"), notConnected: t("integrations:directory.notConnected"), show: t("integrations:directory.show"),
    categories: t("integrations:directory.categories"), results: (q) => t("integrations:directory.results", { q }), noMatch: t("integrations:directory.noMatch"),
    noneInCategory: t("integrations:directory.noneInCategory"), none: t("integrations:directory.none"),
    clearSearch: t("integrations:directory.clearSearch"), clearFilter: t("integrations:directory.clearFilter"),
    connectShort: t("integrations:page.connect"), connectedShort: t("integrations:directory.onlyConnected"), manage: t("integrations:directory.manage"),
  }
  return (
    // the page's own frame and title: the same width as the provider pages; with nothing connected the intro says so
    <Page title={t("integrations:page.title")} sub={n === 0 ? <span data-none-connected>{t("integrations:page.noneSub")}</span> : t("integrations:page.overviewSub")}>
      <div className="flex flex-col" data-integrations data-connections={n}>
        <AppDirectory aria-label={t("integrations:page.title")} apps={apps} categories={categories} labels={labels} onOpen={(a) => go("#/integrations/" + a.id)} />
        <Note>{t("integrations:page.footNote")}</Note>
      </div>
    </Page>
  )
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
  return (
    <Page title={p.name} back={["#/integrations", t("integrations:page.title")]} max={720}
      sub={<Meta><span>{insts.length ? t(p.id === "feishu" ? "integrations:page.connectedApps" : "integrations:page.connectedN", { count: insts.length }) : t("integrations:page.notConnected")}</span>{p.adapters.length > 0 && p.id !== "feishu" && <span>{p.adapters.map((a) => a.name).join(" / ")}</span>}</Meta>}>
      {dlg && <ConnectDialog adapter={asAdapter(dlg, p.id)} onClose={() => setDlg(null)} done={() => go("#/integrations/" + p.id)} />}
      <div className="flex flex-col gap-8" data-provider-page={p.id}>
        {insts.length ? <section><SectionLabel>{t("integrations:page.connectedSection", { n: insts.length })}</SectionLabel>
            <div className="flex flex-col gap-6">{insts.map((i) => <InstallationCard key={i.id} inst={i} p={p} chans={chans} onChange={reload} />)}</div></section>
          : <section><SectionLabel>{t("integrations:page.connectedSection", { n: 0 })}</SectionLabel><Hint data-not-connected>{t("integrations:page.notConnectedHint", { name: p.name })}</Hint></section>}
        <Leftovers p={p} />
        {p.id === "feishu" ? <FeishuGuide adapters={p.adapters} bots={bots} selfDone={p.connections.some((c) => selfIds.includes(c.adapter))} callbackUrls={x.callback_urls} />
          : <WaysIn p={p} onConnect={setDlg} />}
        {!insts.length && <section><SectionLabel>{t("integrations:page.whatItDoes")}</SectionLabel>
          <Group>{[...p.areas].sort((x, y) => +(x.state === "planned") - +(y.state === "planned")).map((a) => <Row key={a.area} title={areaLabel(a.area)} sub={areaNote(p, a.area)} data-area={a.area} data-area-state={a.state}>
            <span className="inline-flex items-center gap-1.5 type-footnote text-muted-foreground"><StatusDot s={a.state === "supported" ? (p.connections.length ? "done" : "idle") : "drop"} />{a.state === "planned" ? t("integrations:page.planned") : p.connections.length ? t("integrations:page.supported") : t("integrations:page.supportedNotConnected")}</span></Row>)}</Group>
          {!p.connections.length && <Note>{t("integrations:page.supportedNote")}</Note>}</section>}
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
