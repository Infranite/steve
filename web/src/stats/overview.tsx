/* 统计页的“现在”（stats v2）：一行细的状态（进行中 · 等你 · 失败或受阻，旁边是 YOLO 运行状态），以及页面共用的下钻明细。
 * “现在”的数字取自事项列表本身，下钻列表用同一个筛选，数字和列表天然一致；
 * “区间”的下钻（每天的产出里点一天）用 `only=done|created` 取同一批事项（steve-stats views::items）。
 * 单日范围（今天）按小时（ST-43）：点一个小时，下钻用 `hour` 取那个小时里完成或新建的事项，和柱子同一个区间。 */
import { useState, type ReactNode } from "react"
import { itemDisplayStatus } from "@/item-workflow-relation"
import { cn } from "cn"
import { ChartColumnIcon, TableIcon, XIcon } from "lucide-react"
import { t, useT } from "@/i18n"
import { ago } from "@/i18n/format"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { st, pendingCards, closedSt, WT, type Item, type Orphan } from "@/store"
import { go, ItemNo, Segmented, StatusDot } from "@/ui"
import type { Filter } from "@/api/stats"
import { STATE } from "@/stats/chart"
import { ItemsTable } from "@/stats/items"
import { bucketName, isHour } from "@/stats/format"

const link = "font-medium text-foreground/80 underline-offset-2 hover:text-foreground hover:underline"

const inScope = (i: Item, f: Filter) => !i.arch && (!f.project || i.project === f.project) && (!f.bot || i.b === f.bot)
const failing = (i: Item) => itemDisplayStatus(i) === "block" || i.waits.some((w) => w.type === "fail")
/** the open items of the page's project / Bot, as the overview counts them */
export function nowSets(f: Filter) {
  const xs = st.items.filter((i) => inScope(i, f) && !closedSt(itemDisplayStatus(i)))
  return {
    run: xs.filter((i) => itemDisplayStatus(i) === "run"),
    cards: pendingCards().filter((c) => inScope(c.i, f)),
    fail: xs.filter(failing),
    by: (k: StatusKey) => xs.filter((i) => GROUPS[k].includes(itemDisplayStatus(i))),
  }
}

export type Drill = "run" | "wait" | "fail" | "done" | `status:${StatusKey}` | `day:${string}`
type StatusKey = "run" | "wait" | "block" | "pause"
const GROUPS: Record<StatusKey, Item["st"][]> = { run: ["run"], wait: ["wait", "prop"], block: ["block"], pause: ["pause"] }
const statusLabel = (k: StatusKey) => (k === "run" ? t("stats:overview.status.run") : k === "wait" ? t("stats:overview.status.wait") : k === "block" ? t("stats:overview.status.block") : t("stats:overview.status.pause"))
const statusEmpty = (k: StatusKey) => (k === "run" ? t("stats:overview.statusEmpty.run") : k === "wait" ? t("stats:overview.statusEmpty.wait") : k === "block" ? t("stats:overview.statusEmpty.block") : t("stats:overview.statusEmpty.pause"))

/** “现在”: what runs, waits for you or failed right now (the item list itself, so a count and its list always agree),
 *  a slim line above the key numbers; each count opens its list. The YOLO run state rides on the same line. */
export function NowLine({ f, drill, setDrill, children }: { f: Filter; drill: Drill | null; setDrill: (d: Drill | null) => void; children?: ReactNode }) {
  const t = useT()
  const now = nowSets(f)
  const toggle = (d: Drill) => setDrill(drill === d ? null : d)
  const n = (k: "run" | "wait" | "fail", label: string, v: number, tone: string, def: string) => (
    <Tooltip key={k}>
      <TooltipTrigger render={<button type="button" onClick={() => toggle(k)} aria-pressed={drill === k} aria-label={t("stats:overview.tileAria", { label, scope: t("stats:overview.now") })}
        className={cn("inline-flex h-7 items-center gap-1.5 rounded-md px-2 type-callout text-muted-foreground outline-none hover:bg-muted hover:text-foreground focus-visible:bg-muted", drill === k && "bg-muted text-foreground")} data-kpi={k} />}>
        <span aria-hidden className={cn("size-2 shrink-0 rounded-full", k === "run" && v > 0 && "dot-running")} style={{ background: v > 0 ? tone : "var(--tertiary)" }} />
        {label}<span className={cn("font-medium tabular-nums", v > 0 ? "text-foreground" : "text-tertiary")}>{v}</span>
      </TooltipTrigger>
      <TooltipContent className="max-w-64">{def}</TooltipContent>
    </Tooltip>
  )
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5" data-now>
      <span className="inline-flex items-center gap-0.5" data-now-counts>
        <span className="mr-1.5 type-label text-tertiary">{t("stats:overview.now")}</span>
        {n("run", t("stats:overview.run"), now.run.length, STATE.run, t("stats:overview.runDef"))}
        {n("wait", t("stats:v2.now.wait"), now.cards.length, STATE.wait, t("stats:overview.waitDef"))}
        {n("fail", t("stats:overview.fail"), now.fail.length, STATE.fail, t("stats:overview.failDef"))}
      </span>
      <span className="flex-1" />
      {children}
    </div>
  )
}

/* ───────── the drill-down ───────── */

function ItemLines({ rows, empty }: { rows: { i: Item; sub?: ReactNode; key?: string }[]; empty: string }) {
  const t = useT()
  const [all, setAll] = useState(false)
  if (!rows.length) return <div className="py-6 type-callout text-muted-foreground anim-in">{empty}</div>
  const shown = all ? rows : rows.slice(0, 12)
  return (
    <div className="flex flex-col" data-drill-list>
      {shown.map(({ i, sub, key }) => (
        <button key={key ?? i.id} type="button" onClick={() => go(i.id ? "#/items/" + i.id : "#/inbox")} className="flex w-full min-w-0 items-center gap-3 rounded-lg px-2 py-2 text-left hover:bg-muted focus-visible:bg-muted focus-visible:outline-none" data-drill-row={i.id || key}>
          <StatusDot s={i.st} />{i.id && <ItemNo id={i.id} />}
          <span className="min-w-0 flex-1 truncate type-body">{i.t}{sub && <span className="ml-2 type-callout text-muted-foreground">{sub}</span>}</span>
          <span className="shrink-0 type-footnote tabular-nums text-muted-foreground max-sm:hidden">{ago(i.updated)}</span>
        </button>
      ))}
      {rows.length > shown.length && <Button size="xs" variant="ghost" className="mt-1 self-start text-muted-foreground" onClick={() => setAll(true)}>{t("stats:overview.showMore", { count: rows.length - shown.length })}</Button>}
    </div>
  )
}

const cardLine = (c: Orphan) => ({ i: c.i, key: c.w.decision, sub: <>{WT[c.w.type] ?? c.w.type} · {c.w.q}</> })

export function DrillPanel({ d, f, fkey, live, rangeName, close }: { d: Drill; f: Filter; fkey: string; live: boolean; rangeName: string; close: () => void }) {
  const t = useT()
  const now = nowSets(f)
  const [dayTab, setDayTab] = useState<"done" | "created">("done")
  const toItems = (x: "全部" | "等你" | "进行中") => void import("@/views").then((m) => m.showItems(x, f.project, f.bot))
  let title: ReactNode, body: ReactNode, more: ReactNode = null
  if (d === "run" || d === "status:run") {
    title = t("stats:overview.drillRun", { count: now.run.length }); body = <ItemLines rows={now.run.map((i) => ({ i, sub: i.now }))} empty={t("stats:overview.statusEmpty.run")} />
    more = <button type="button" className={link} onClick={() => toItems("进行中")}>{t("stats:overview.viewInItems")}</button>
  } else if (d === "wait") {
    title = t("stats:overview.drillWait", { count: now.cards.length }); body = <ItemLines rows={now.cards.map(cardLine)} empty={t("stats:overview.noneWaitingFull")} />
    more = <a href="#/inbox" className={link}>{t("stats:overview.toInbox")}</a>
  } else if (d === "fail") {
    title = t("stats:overview.drillFail", { count: now.fail.length })
    body = <ItemLines rows={now.fail.map((i) => { const w = i.waits.find((x) => x.type === "fail"); return { i, sub: w ? t("stats:overview.failSub", { q: w.q }) : t("stats:overview.blockedSub", { deps: (i.deps ?? []).join(t("stats:word.listSep")) || t("stats:overview.blockedDeps") }) } })} empty={t("stats:overview.failEmpty")} />
  } else if (d.startsWith("status:")) {
    const k = d.slice(7) as StatusKey, xs = now.by(k), label = statusLabel(k)
    title = t("stats:overview.drillStatus", { label, count: xs.length }); body = <ItemLines rows={xs.map((i) => ({ i, sub: k === "wait" ? (i.wait ? `${WT[i.wait.type] ?? ""} · ${i.wait.q}` : t("stats:overview.proposal")) : i.now }))} empty={statusEmpty(k)} />
    if (k === "wait") more = <button type="button" className={link} onClick={() => toItems("等你")}>{t("stats:overview.viewInItems")}</button>
  } else if (d === "done") {
    title = t("stats:overview.drillDone", { range: rangeName }); body = <ItemsTable f={f} fkey={fkey + "done"} live={live} only="done" sort0="done_at" />
  } else {
    /* a day, or an hour of the day (`YYYY-MM-DDTHH:00`): the same span its bar counts */
    const b = d.slice(4), day = b.slice(0, 10)
    const g: Filter = { ...f, range: "custom", from: day, to: day, hour: isHour(b) ? Number(b.slice(11, 13)) : undefined }
    title = <span className="inline-flex flex-wrap items-center gap-3">{t("stats:overview.drillDay", { bucket: bucketName(b) })}<Segmented size="sm" value={dayTab} onChange={setDayTab} options={[["done", t("stats:overview.completed")], ["created", t("stats:overview.created")]] as const} /></span>
    body = <ItemsTable key={dayTab} f={g} fkey={fkey + d + dayTab} live={live} only={dayTab} sort0={dayTab === "done" ? "done_at" : "est"} />
  }
  return (
    <section className="anim-in mx-2 mb-2 rounded-lg bg-muted/60 px-3 pt-3 pb-2 border-t-0!" aria-label={t("stats:overview.drillAria")} data-drill={d}>
      <div className="mb-1 flex items-center gap-2 px-2">
        <h3 className="min-w-0 flex-1 type-body font-medium">{title}</h3>
        {more && <span className="type-callout">{more}</span>}
        <Button size="icon-xs" variant="ghost" className="text-muted-foreground" aria-label={t("stats:overview.drillClose")} onClick={close} data-drill-close><XIcon /></Button>
      </div>
      <div className={cn(d === "done" || d.startsWith("day:") ? "-mx-3" : undefined)}>{body}</div>
    </section>
  )
}

/* ───────── small shared pieces ───────── */

export function ModeSwitch({ table, set }: { table: boolean; set: (v: boolean) => void }) {
  const t = useT()
  return <Button variant="ghost" size="xs" className="text-muted-foreground" onClick={() => set(!table)} aria-pressed={table} data-chart-mode>{table ? <><ChartColumnIcon />{t("stats:overview.chart")}</> : <><TableIcon />{t("stats:overview.table")}</>}</Button>
}
export function Loading({ err, h = 200 }: { err?: string | null; h?: number }) {
  return <div className="flex items-center justify-center type-callout text-muted-foreground" style={{ height: h }} role={err ? "alert" : "status"}>{err ?? <Spinner className="size-4 text-muted-foreground" />}</div>
}
