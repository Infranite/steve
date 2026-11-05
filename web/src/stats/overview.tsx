/* 统计页的概览（ST-40）：运行状态（原来顶部的 YOLO 横栏，见 yolo/status.tsx）、四个核心指标、新建与完成的趋势、进行中事项的状态分布，
 * 以及它们共用的下钻明细。“现在”的数字取自事项列表本身，下钻列表用同一个筛选，数字和列表天然一致；
 * “区间”的数字取自统计接口，下钻用 `only=done|created` 取同一批事项（steve-stats views::items）。
 * 单日范围（今天）按小时（ST-43）：点一个小时，下钻用 `hour` 取那个小时里完成或新建的事项，和柱子同一个区间。 */
import { useState, type ReactNode } from "react"
import { cn } from "cn"
import { ChartColumnIcon, TableIcon, XIcon } from "lucide-react"
import { Trans } from "react-i18next"
import { t, useT } from "@/i18n"
import { ago } from "@/i18n/format"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { st, pendingCards, WT, type Item, type Orphan } from "@/store"
import { go, ItemNo, Segmented, StatusDot } from "@/ui"
import { stats, type Filter, type Series, type Summary } from "@/api/stats"
import { Chart, ChartHead, ChartTable, HBars, STATE, type BarRow } from "@/stats/chart"
import { ItemsTable } from "@/stats/items"
import { bucketName, delta, fmtRange, isHour, oneDay, zoneName, useStats } from "@/stats/format"

const link = "font-medium text-foreground/80 underline-offset-2 hover:text-foreground hover:underline"

const inScope = (i: Item, f: Filter) => !i.arch && (!f.project || i.project === f.project) && (!f.bot || i.b === f.bot)
const failing = (i: Item) => i.st === "block" || i.waits.some((w) => w.type === "fail")
/** the open items of the page's project / Bot, as the overview counts them */
export function nowSets(f: Filter) {
  const xs = st.items.filter((i) => inScope(i, f) && i.st !== "done" && i.st !== "drop")
  return {
    run: xs.filter((i) => i.st === "run"),
    cards: pendingCards().filter((c) => inScope(c.i, f)),
    fail: xs.filter(failing),
    by: (k: StatusKey) => xs.filter((i) => GROUPS[k].includes(i.st)),
  }
}

export type Drill = "run" | "wait" | "fail" | "done" | `status:${StatusKey}` | `day:${string}`
type StatusKey = "run" | "wait" | "block" | "pause"
const GROUPS: Record<StatusKey, Item["st"][]> = { run: ["run"], wait: ["wait", "prop"], block: ["block"], pause: ["pause"] }
const STATUS: [StatusKey, string][] = [["run", STATE.run], ["wait", STATE.wait], ["block", STATE.idle], ["pause", STATE.idle]]
const statusLabel = (k: StatusKey) => (k === "run" ? t("stats:overview.status.run") : k === "wait" ? t("stats:overview.status.wait") : k === "block" ? t("stats:overview.status.block") : t("stats:overview.status.pause"))
const statusEmpty = (k: StatusKey) => (k === "run" ? t("stats:overview.statusEmpty.run") : k === "wait" ? t("stats:overview.statusEmpty.wait") : k === "block" ? t("stats:overview.statusEmpty.block") : t("stats:overview.statusEmpty.pause"))

function Tile({ k, label, scope, value, foot, def, tone, on, onClick }: { k: string; label: string; scope: string; value: ReactNode; foot?: ReactNode; def: string; tone: string; on: boolean; onClick: () => void }) {
  const t = useT()
  return (
    <Tooltip>
      <TooltipTrigger render={<button type="button" onClick={onClick} aria-pressed={on} aria-label={t("stats:overview.tileAria", { label, scope })}
        className={cn("min-w-0 rounded-lg px-3 py-2.5 text-left outline-none transition-colors hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-ring", on && "bg-muted")} data-kpi={k} />}>
        <span className="flex items-center gap-1.5 type-label text-muted-foreground"><span aria-hidden className="size-2 shrink-0 rounded-full" style={{ background: tone }} />{label}<span className="font-normal text-tertiary">· {scope}</span></span>
        <span className="mt-1 block truncate text-[26px] font-semibold leading-8 tracking-[-0.02em] text-foreground">{value}</span>
        <span className="mt-0.5 block min-h-4 truncate type-footnote text-muted-foreground">{foot}</span>
      </TooltipTrigger>
      <TooltipContent className="max-w-64">{def}</TooltipContent>
    </Tooltip>
  )
}

/** the four figures that matter, each opening its list */
export function CoreKpis({ s, f, prevName, rangeName, drill, setDrill }: { s: Summary; f: Filter; prevName: string; rangeName: string; drill: Drill | null; setDrill: (d: Drill | null) => void }) {
  const t = useT()
  const now = nowSets(f)
  const toggle = (d: Drill) => setDrill(drill === d ? null : d)
  const d = delta(s.throughput.done, s.prev?.throughput.done)
  return (
    <div className="-mx-3 grid grid-cols-2 gap-1 lg:grid-cols-4" data-core-kpis>
      <Tile k="run" label={t("stats:overview.run")} scope={t("stats:overview.now")} value={now.run.length} tone={STATE.run} foot={t("stats:overview.runFoot", { count: now.by("run").length + now.by("wait").length + now.by("block").length + now.by("pause").length })}
        def={t("stats:overview.runDef")} on={drill === "run"} onClick={() => toggle("run")} />
      <Tile k="wait" label={t("stats:overview.wait")} scope={t("stats:overview.now")} value={now.cards.length} tone={STATE.wait} foot={now.cards.length ? t("stats:overview.waitFoot", { count: new Set(now.cards.map((c) => c.i.id || c.w.decision)).size }) : t("stats:overview.noneWaiting")}
        def={t("stats:overview.waitDef")} on={drill === "wait"} onClick={() => toggle("wait")} />
      <Tile k="done" label={t("stats:overview.done")} scope={rangeName} value={s.throughput.done} tone={STATE.done} foot={d ? t("stats:delta.vs", { prev: prevName, delta: d }) : s.prev ? t("stats:overview.prevCount", { prev: prevName, count: s.prev.throughput.done }) : ""}
        def={t("stats:overview.doneDef")} on={drill === "done"} onClick={() => toggle("done")} />
      <Tile k="fail" label={t("stats:overview.fail")} scope={t("stats:overview.now")} value={now.fail.length} tone={STATE.fail} foot={now.fail.length ? t("stats:overview.failFoot", { fails: now.fail.filter((i) => i.waits.some((w) => w.type === "fail")).length, blocked: now.fail.filter((i) => i.st === "block").length }) : t("stats:overview.none")}
        def={t("stats:overview.failDef")} on={drill === "fail"} onClick={() => toggle("fail")} />
    </div>
  )
}

/** the meta line of a chart over the page's range: “件 / 天 · 11月1日 – 11月7日”, or by hour “件 / 小时 · 11月5日 00:00–24:00 · 北京时间” */
export const rangeMeta = (unit: string, s: Summary, hourly: boolean) =>
  hourly ? t("stats:overview.metaHourly", { unit, range: fmtRange(s.range.from, s.range.to), zone: zoneName(s.range.tz) }) : t("stats:overview.metaDaily", { unit, range: fmtRange(s.range.from, s.range.to) })

/** 新建与完成, per day (per hour for one day), side by side */
export function FlowChart({ s, f, fkey, live, drill, setDrill }: { s: Summary; f: Filter; fkey: string; live: boolean; drill: Drill | null; setDrill: (d: Drill | null) => void }) {
  const t = useT()
  const hourly = oneDay(f)
  const [cr] = useStats(() => stats.series(f, "created", "none", hourly), [fkey], live)
  const [dn] = useStats(() => stats.series(f, "done", "none", hourly), [fkey], live)
  const [table, setTable] = useState(false)
  /* null stays null: an hour not yet come is empty, not 0 */
  const val = (v: number | null | undefined) => (v === null ? null : v ?? 0)
  const data: Series | null = cr.s === "ok" && dn.s === "ok" ? {
    ...cr.v,
    series: [
      { key: "created", label: t("stats:overview.created"), values: cr.v.buckets.map((_, i) => val(cr.v.series[0]?.values[i])) },
      { key: "done", label: t("stats:overview.completed"), values: cr.v.buckets.map((b) => { const j = dn.v.buckets.indexOf(b); return j < 0 ? 0 : val(dn.v.series[0]?.values[j]) }) },
    ],
  } : null
  const picked = drill?.startsWith("day:") ? drill.slice(4) : null
  const pick = (day: string | null) => setDrill(day ? `day:${day}` : null)
  const err = cr.s === "error" ? cr.msg : dn.s === "error" ? dn.msg : null
  return (
    <section data-flow-chart>
      <ChartHead k="flow" title={t("stats:overview.flow")} meta={rangeMeta(t("stats:overview.unitItems"), s, hourly)}
        legend={[{ label: t("stats:overview.flowCreated", { count: s.throughput.created }), c: STATE.created }, { label: t("stats:overview.flowDone", { count: s.throughput.done }), c: STATE.done }, ...(hourly && data?.current != null ? [{ label: t("stats:overview.flowCurrent"), c: "var(--muted)" }] : [])]}
        right={<ModeSwitch table={table} set={setTable} />} />
      {!data ? <Loading err={err} /> : table ? <ChartTable data={data} fmt={(v) => String(Math.round(v))} picked={picked} onPick={pick} />
        : <Chart data={data} kind="group" int fmt={(v) => String(Math.round(v))} colors={[STATE.created, STATE.done]} picked={picked} onPick={pick} label={hourly ? t("stats:overview.flowAriaHourly") : t("stats:overview.flowAriaDaily")} height={200} />}
      {data && data.series.every((l) => l.values.every((v) => !v)) && <div className="mt-2 type-footnote text-muted-foreground">{t("stats:overview.flowEmpty")}</div>}
    </section>
  )
}

/** the open items by status, now */
export function StatusBars({ f, drill, setDrill }: { f: Filter; drill: Drill | null; setDrill: (d: Drill | null) => void }) {
  const t = useT()
  const now = nowSets(f)
  const rows: BarRow[] = STATUS.map(([k, c]) => ({ key: k, label: statusLabel(k), c, value: now.by(k).length }))
  const total = rows.reduce((a, r) => a + r.value, 0)
  const fails = now.fail.filter((i) => i.waits.some((w) => w.type === "fail")).length
  return (
    <section data-status-bars>
      <ChartHead k="status" title={t("stats:overview.statusTitle")} meta={t("stats:overview.statusMeta", { count: total })} />
      {total === 0 ? <div className="py-8 text-center type-callout text-muted-foreground">{t("stats:overview.statusNone")}</div>
        : <HBars k="status" label={t("stats:overview.statusAria")} rows={rows} total={Math.max(...rows.map((r) => r.value))} picked={drill?.startsWith("status:") ? drill.slice(7) : null} onPick={(k) => setDrill(drill === `status:${k}` ? null : `status:${k}` as Drill)} />}
      {fails > 0 && <div className="mt-2 type-footnote text-muted-foreground"><Trans t={t} i18nKey="stats:overview.failsNote" count={fails} components={{ link: <button type="button" className={link} onClick={() => setDrill("fail")} /> }} /></div>}
    </section>
  )
}

/* ───────── the drill-down ───────── */

function ItemLines({ rows, empty }: { rows: { i: Item; sub?: ReactNode; key?: string }[]; empty: string }) {
  const t = useT()
  const [all, setAll] = useState(false)
  if (!rows.length) return <div className="py-6 type-callout text-muted-foreground">{empty}</div>
  const shown = all ? rows : rows.slice(0, 12)
  return (
    <div className="flex flex-col" data-drill-list>
      {shown.map(({ i, sub, key }) => (
        <button key={key ?? i.id} type="button" onClick={() => go(i.id ? "#/items/" + i.id : "#/inbox")} className="flex w-full min-w-0 items-center gap-3 border-b px-1 py-2 text-left last:border-0 hover:bg-muted/50 focus-visible:bg-muted/50 focus-visible:outline-none" data-drill-row={i.id || key}>
          <StatusDot s={i.st} />{i.id && <ItemNo id={i.id} />}
          <span className="min-w-0 flex-1 truncate type-callout">{i.t}{sub && <span className="ml-2 type-footnote text-muted-foreground">{sub}</span>}</span>
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
    <section className="rounded-xl border px-4 pt-3 pb-2" aria-label={t("stats:overview.drillAria")} data-drill={d}>
      <div className="mb-2 flex items-center gap-2">
        <h3 className="min-w-0 flex-1 type-headline">{title}</h3>
        {more && <span className="type-footnote">{more}</span>}
        <Button size="icon-xs" variant="ghost" className="text-muted-foreground" aria-label={t("stats:overview.drillClose")} onClick={close} data-drill-close><XIcon /></Button>
      </div>
      {body}
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
