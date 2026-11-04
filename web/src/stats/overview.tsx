/* 统计页的概览（ST-40）：运行状态（原来顶部的 YOLO 横栏，见 yolo/status.tsx）、四个核心指标、新建与完成的趋势、进行中事项的状态分布，
 * 以及它们共用的下钻明细。“现在”的数字取自事项列表本身，下钻列表用同一个筛选，数字和列表天然一致；
 * “区间”的数字取自统计接口，下钻用 `only=done|created` 取同一批事项（steve-stats views::items）。 */
import { useState, type ReactNode } from "react"
import { cn } from "cn"
import { ChartColumnIcon, TableIcon, XIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { st, pendingCards, WT, type Item, type Orphan } from "@/store"
import { go, ItemNo, Segmented, StatusDot } from "@/ui"
import { stats, type Filter, type Series, type Summary } from "@/api/stats"
import { Chart, ChartHead, ChartTable, HBars, STATE, type BarRow } from "@/stats/chart"
import { ItemsTable } from "@/stats/items"
import { delta, fmtRange, shortDay, useStats } from "@/stats/format"

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
const STATUS: [StatusKey, string, string][] = [["run", "进行中", STATE.run], ["wait", "等你", STATE.wait], ["block", "等前置", STATE.idle], ["pause", "已暂停", STATE.idle]]

function Tile({ k, label, scope, value, foot, def, tone, on, onClick }: { k: string; label: string; scope: string; value: ReactNode; foot?: ReactNode; def: string; tone: string; on: boolean; onClick: () => void }) {
  return (
    <Tooltip>
      <TooltipTrigger render={<button type="button" onClick={onClick} aria-pressed={on} aria-label={`${label}（${scope}），查看明细`}
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
  const now = nowSets(f)
  const toggle = (d: Drill) => setDrill(drill === d ? null : d)
  const d = delta(s.throughput.done, s.prev?.throughput.done)
  return (
    <div className="-mx-3 grid grid-cols-2 gap-1 lg:grid-cols-4" data-core-kpis>
      <Tile k="run" label="进行中" scope="现在" value={now.run.length} tone={STATE.run} foot={`共 ${now.by("run").length + now.by("wait").length + now.by("block").length + now.by("pause").length} 件没结束`}
        def="现在状态是“进行中”的事项（不含归档）。“没结束”还包括等你、等前置和已暂停的。" on={drill === "run"} onClick={() => toggle("run")} />
      <Tile k="wait" label="待你处理" scope="现在" value={now.cards.length} tone={STATE.wait} foot={now.cards.length ? `${new Set(now.cards.map((c) => c.i.id || c.w.decision)).size} 件事项在等` : "没有事卡在你这里"}
        def="待处理页里还开着的卡片：验收、审批、问题、异常等，不含稍后提醒的。" on={drill === "wait"} onClick={() => toggle("wait")} />
      <Tile k="done" label="已完成" scope={rangeName} value={s.throughput.done} tone={STATE.done} foot={d ? `比${prevName} ${d}` : s.prev ? `${prevName} ${s.prev.throughput.done} 件` : ""}
        def="区间内状态变成“完成”的事项数；同一件事项在一个区间里只算一次。" on={drill === "done"} onClick={() => toggle("done")} />
      <Tile k="fail" label="失败或受阻" scope="现在" value={now.fail.length} tone={STATE.fail} foot={now.fail.length ? `异常卡 ${now.fail.filter((i) => i.waits.some((w) => w.type === "fail")).length} · 等前置 ${now.fail.filter((i) => i.st === "block").length}` : "没有"}
        def="现在有未处理的异常卡（失败、停滞、发布失败等），或在等前置事项的事项。" on={drill === "fail"} onClick={() => toggle("fail")} />
    </div>
  )
}

/** 新建与完成, per day, side by side */
export function FlowChart({ s, f, fkey, live, drill, setDrill }: { s: Summary; f: Filter; fkey: string; live: boolean; drill: Drill | null; setDrill: (d: Drill | null) => void }) {
  const [cr] = useStats(() => stats.series(f, "created", "none"), [fkey], live)
  const [dn] = useStats(() => stats.series(f, "done", "none"), [fkey], live)
  const [table, setTable] = useState(false)
  const data: Series | null = cr.s === "ok" && dn.s === "ok" ? {
    buckets: cr.v.buckets,
    series: [
      { key: "created", label: "新建", values: cr.v.buckets.map((_, i) => cr.v.series[0]?.values[i] ?? 0) },
      { key: "done", label: "完成", values: cr.v.buckets.map((b) => { const j = dn.v.buckets.indexOf(b); return j < 0 ? 0 : dn.v.series[0]?.values[j] ?? 0 }) },
    ],
  } : null
  const picked = drill?.startsWith("day:") ? drill.slice(4) : null
  const pick = (day: string | null) => setDrill(day ? `day:${day}` : null)
  const err = cr.s === "error" ? cr.msg : dn.s === "error" ? dn.msg : null
  return (
    <section data-flow-chart>
      <ChartHead k="flow" title="新建与完成" meta={`件 / 天 · ${fmtRange(s.range.from, s.range.to)}`}
        legend={[{ label: `新建 ${s.throughput.created} 件`, c: STATE.created }, { label: `完成 ${s.throughput.done} 件`, c: STATE.done }]}
        right={<ModeSwitch table={table} set={setTable} />} />
      {!data ? <Loading err={err} /> : table ? <ChartTable data={data} fmt={(v) => String(Math.round(v))} picked={picked} onPick={pick} />
        : <Chart data={data} kind="group" int fmt={(v) => String(Math.round(v))} colors={[STATE.created, STATE.done]} picked={picked} onPick={pick} label="每天新建与完成的事项数" height={200} />}
      {data && data.series.every((l) => l.values.every((v) => !v)) && <div className="mt-2 type-footnote text-muted-foreground">这段时间没有新建或完成的事项。</div>}
    </section>
  )
}

/** the open items by status, now */
export function StatusBars({ f, drill, setDrill }: { f: Filter; drill: Drill | null; setDrill: (d: Drill | null) => void }) {
  const now = nowSets(f)
  const rows: BarRow[] = STATUS.map(([k, label, c]) => ({ key: k, label, c, value: now.by(k).length }))
  const total = rows.reduce((a, r) => a + r.value, 0)
  const fails = now.fail.filter((i) => i.waits.some((w) => w.type === "fail")).length
  return (
    <section data-status-bars>
      <ChartHead k="status" title="没结束的事项 · 按状态" meta={`件 · 现在，共 ${total} 件（不含归档、已完成、已放弃）`} />
      {total === 0 ? <div className="py-8 text-center type-callout text-muted-foreground">现在没有没结束的事项。</div>
        : <HBars k="status" label="没结束的事项按状态" rows={rows} total={Math.max(...rows.map((r) => r.value))} picked={drill?.startsWith("status:") ? drill.slice(7) : null} onPick={(k) => setDrill(drill === `status:${k}` ? null : `status:${k}` as Drill)} />}
      {fails > 0 && <div className="mt-2 type-footnote text-muted-foreground">其中 {fails} 件挂着异常卡，<button type="button" className={link} onClick={() => setDrill("fail")}>看失败或受阻</button></div>}
    </section>
  )
}

/* ───────── the drill-down ───────── */

const ago = (ms: number) => { const m = Math.round((Date.now() - ms) / 60000); return m < 1 ? "刚刚" : m < 60 ? `${m} 分钟前` : m < 1440 ? `${Math.round(m / 60)} 小时前` : `${Math.round(m / 1440)} 天前` }
function ItemLines({ rows, empty }: { rows: { i: Item; sub?: ReactNode; key?: string }[]; empty: string }) {
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
      {rows.length > shown.length && <Button size="xs" variant="ghost" className="mt-1 self-start text-muted-foreground" onClick={() => setAll(true)}>再显示 {rows.length - shown.length} 件</Button>}
    </div>
  )
}

const cardLine = (c: Orphan) => ({ i: c.i, key: c.w.decision, sub: <>{WT[c.w.type] ?? c.w.type} · {c.w.q}</> })

export function DrillPanel({ d, f, fkey, live, rangeName, close }: { d: Drill; f: Filter; fkey: string; live: boolean; rangeName: string; close: () => void }) {
  const now = nowSets(f)
  const [dayTab, setDayTab] = useState<"done" | "created">("done")
  const toItems = (x: "全部" | "等你" | "进行中") => void import("@/views").then((m) => m.showItems(x, f.project, f.bot))
  let title: ReactNode, body: ReactNode, more: ReactNode = null
  if (d === "run" || d === "status:run") {
    title = `进行中 · ${now.run.length} 件`; body = <ItemLines rows={now.run.map((i) => ({ i, sub: i.now }))} empty="现在没有进行中的事项。" />
    more = <button type="button" className={link} onClick={() => toItems("进行中")}>在事项页查看</button>
  } else if (d === "wait") {
    title = `待你处理 · ${now.cards.length} 张卡`; body = <ItemLines rows={now.cards.map(cardLine)} empty="没有事卡在你这里。" />
    more = <a href="#/inbox" className={link}>去待处理</a>
  } else if (d === "fail") {
    title = `失败或受阻 · ${now.fail.length} 件`
    body = <ItemLines rows={now.fail.map((i) => { const w = i.waits.find((x) => x.type === "fail"); return { i, sub: w ? `异常：${w.q}` : `等前置：${(i.deps ?? []).join("、") || "前置事项"}` } })} empty="现在没有失败或受阻的事项。" />
  } else if (d.startsWith("status:")) {
    const k = d.slice(7) as StatusKey, xs = now.by(k), label = STATUS.find((x) => x[0] === k)![1]
    title = `${label} · ${xs.length} 件`; body = <ItemLines rows={xs.map((i) => ({ i, sub: k === "wait" ? (i.wait ? `${WT[i.wait.type] ?? ""} · ${i.wait.q}` : "提议") : i.now }))} empty={`现在没有${label}的事项。`} />
    if (k === "wait") more = <button type="button" className={link} onClick={() => toItems("等你")}>在事项页查看</button>
  } else if (d === "done") {
    title = `已完成 · ${rangeName}`; body = <ItemsTable f={f} fkey={fkey + "done"} live={live} only="done" sort0="done_at" />
  } else {
    const day = d.slice(4), g: Filter = { ...f, range: "custom", from: day, to: day }
    title = <span className="inline-flex flex-wrap items-center gap-3">{shortDay(day)} 的事项<Segmented size="sm" value={dayTab} onChange={setDayTab} options={[["done", "完成"], ["created", "新建"]] as const} /></span>
    body = <ItemsTable key={dayTab} f={g} fkey={fkey + d + dayTab} live={live} only={dayTab} sort0={dayTab === "done" ? "done_at" : "est"} />
  }
  return (
    <section className="rounded-xl border px-4 pt-3 pb-2" aria-label="明细" data-drill={d}>
      <div className="mb-2 flex items-center gap-2">
        <h3 className="min-w-0 flex-1 type-headline">{title}</h3>
        {more && <span className="type-footnote">{more}</span>}
        <Button size="icon-xs" variant="ghost" className="text-muted-foreground" aria-label="收起明细" onClick={close} data-drill-close><XIcon /></Button>
      </div>
      {body}
    </section>
  )
}

/* ───────── small shared pieces ───────── */

export function ModeSwitch({ table, set }: { table: boolean; set: (v: boolean) => void }) {
  return <Button variant="ghost" size="xs" className="text-muted-foreground" onClick={() => set(!table)} aria-pressed={table} data-chart-mode>{table ? <><ChartColumnIcon />图表</> : <><TableIcon />表格</>}</Button>
}
export function Loading({ err, h = 200 }: { err?: string | null; h?: number }) {
  return <div className="flex items-center justify-center type-callout text-muted-foreground" style={{ height: h }} role={err ? "alert" : "status"}>{err ?? <Spinner className="size-4 text-muted-foreground" />}</div>
}
