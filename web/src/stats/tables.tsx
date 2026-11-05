/* 统计页的分节（stats.md §6.3 第 4–7 条，ST-40 重整）：耗时、用量明细、质量与介入、Agent。
 * 不加卡片框，靠字号、留白和细分隔线分层；分布用横向条形（HBars），趋势用图；数字 tabular-nums。
 * 颜色同概览：完成绿、等你橙、失败红、Agent 工作蓝，其余灰；“谁”的分组用 SERIES。 */
import { useState, type ReactNode } from "react"
import { cn } from "cn"
import { ChevronDownIcon, ChevronRightIcon } from "lucide-react"
import { stats, type CountRow, type Filter, type Summary, type UsageRow } from "@/api/stats"
import { Segmented } from "@/ui"
import { Spinner } from "@/components/ui/spinner"
import { Chart, ChartHead, ChartTable, HBars, Legend, Share, STATE, SERIES } from "@/stats/chart"
import { Est, Kpi } from "@/stats/kpi"
import { Loading, ModeSwitch } from "@/stats/overview"
import { bucketName, durUnit, fmtMs, fmtN, fmtPct, fmtUsd, isHour, oneDay, zoneName, useStats } from "@/stats/format"
import { harnessName } from "@/model-picker"
import { t, useT } from "@/i18n"

export const th = "py-2 font-medium text-muted-foreground"
export const num = "py-2 pl-4 text-right tabular-nums"

/** a part of the page: its title, what it covers, then its charts */
export function Section({ title, meta, children, k, right }: { title: ReactNode; meta?: ReactNode; children: ReactNode; k: string; right?: ReactNode }) {
  return (
    <section className="flex flex-col gap-6" data-section={k}>
      <div className="flex items-end justify-between gap-3 border-b pb-2">
        <div className="min-w-0"><h2 className="text-[17px] font-semibold leading-6 tracking-[-0.01em]">{title}</h2>{meta && <div className="type-footnote text-muted-foreground">{meta}</div>}</div>
        {right}
      </div>
      {children}
    </section>
  )
}

/** a hairline table that scrolls sideways on a phone */
export function Table({ head, children, label, min = 560 }: { head: ReactNode; children: ReactNode; label: string; min?: number }) {
  return (
    <div className="-mx-4 overflow-x-auto px-4 sm:mx-0 sm:px-0">
      <table className="w-full type-callout" style={{ minWidth: min }} aria-label={label}>
        <thead className="border-b text-left type-footnote">{head}</thead>
        <tbody className="[&>tr]:border-b [&>tr:last-child]:border-0">{children}</tbody>
      </table>
    </div>
  )
}

type UsageTab = "model" | "bot" | "project" | "purpose"
const USAGE_TABS = () => [["model", t("stats:usage.tab.model")], ["bot", "Bot"], ["project", t("stats:usage.tab.project")], ["purpose", t("stats:usage.tab.purpose")]] as const

/** 用量明细: one table per dimension, or the item table */
export function UsageSection({ f, fkey, live, picked }: { f: Filter; fkey: string; live: boolean; picked: string | null }) {
  const t = useT()
  const [tab, setTab] = useState<UsageTab>("model")
  /* a picked day, or a picked hour of the day (ST-43) */
  const day = picked?.slice(0, 10)
  const g: Filter = picked ? { ...f, range: "custom", from: day, to: day, hour: isHour(picked) ? Number(picked.slice(11, 13)) : undefined } : f
  return (
    <section data-usage>
      <ChartHead k="usage-table" title={t("stats:usage.title")} meta={picked ? <span className="text-blue">{t("stats:usage.only", { bucket: bucketName(picked) })}</span> : t("stats:usage.meta")} right={<Segmented size="sm" value={tab} onChange={setTab} options={USAGE_TABS()} />} />
      <UsageTable f={g} by={tab} fkey={fkey + picked} live={live} />
    </section>
  )
}

function UsageTable({ f, by, fkey, live }: { f: Filter; by: UsageTab; fkey: string; live: boolean }) {
  const t = useT()
  const [d] = useStats(() => stats.usage(f, by), [fkey, by], live)
  if (d.s === "loading") return <div className="flex h-24 items-center justify-center"><Spinner className="size-4 text-muted-foreground" /></div>
  if (d.s !== "ok") return <div className="py-6 type-callout text-muted-foreground" role="alert">{d.s === "error" ? d.msg : t("stats:usage.loadFailed")}</div>
  const rows = d.v.rows.filter((r) => r.total > 0 || r.est > 0 || r.reported > 0 || r.turns > 0 || r.calls > 0)
  if (!rows.length) return <div className="py-6 type-callout text-muted-foreground" data-usage-empty>{t("stats:usage.empty")}</div>
  const est = rows.reduce((a, r) => a + r.est, 0), tok = rows.reduce((a, r) => a + r.total, 0)
  const share = (r: UsageRow) => (est > 0 ? r.est / est : tok > 0 ? r.total / tok : 0)
  return (
    <Table label={t("stats:usage.title")} min={720} head={<tr><th className={th}>{t("stats:col.name")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.input")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.output")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.cacheRead")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.cacheWrite")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.hitRate")}</th><th className={cn(th, "pl-4 text-right")}><span className="inline-flex items-center gap-1">{t("stats:word.estCost")}<Est /></span></th><th className={cn(th, "w-24 pl-4")}>{t("stats:col.share")}</th></tr>}>
      {rows.map((r) => (
        <tr key={r.key} data-usage-row={r.key}>
          <td className="max-w-56 truncate py-2 pr-2" title={r.key}>{r.label}{by === "model" && r.key && !r.priced && <span className="ml-1 type-footnote text-muted-foreground">{t("stats:usage.unpricedTag")}</span>}
            {by === "purpose" && !!r.models?.length && <span className="block truncate text-[11.5px] text-muted-foreground" title={r.models.map((m) => `${m.harness ? harnessName(m.harness) + " · " : ""}${m.model} ×${m.calls}`).join(t("stats:word.commaSep"))} data-purpose-models>{r.models.map((m, i) => <span key={i}>{i > 0 && " · "}{m.harness && (i === 0 || m.harness !== r.models![i - 1].harness) ? `${harnessName(m.harness)} ` : ""}<span className="font-mono">{m.model.replace(/^claude-/, "")}</span>{r.models!.length > 1 ? ` ×${m.calls}` : ""}</span>)}</span>}</td>
          <td className={num}>{fmtN(r.input)}</td><td className={num}>{fmtN(r.output)}</td><td className={num}>{fmtN(r.cache_read)}</td><td className={num}>{fmtN(r.cache_write)}</td>
          <td className={num}>{fmtPct(r.hit_rate)}</td>
          <td className={num}>{r.total === 0 && r.reported > 0 ? <span className="text-muted-foreground" title={t("stats:word.sessionQuoteIsFloor")}>{t("stats:word.quoteFloor", { usd: fmtUsd(r.reported) })}</span> : by === "model" && r.key && !r.priced ? <a href="#/settings/stats" className="text-blue hover:underline" data-unpriced>{t("stats:word.unpriced")}</a> : <span title={[r.price_source ? (r.price_source === "models_dev" ? t("stats:usage.fromMd") : t("stats:usage.fromTable")) : "", r.reported ? t("stats:word.adapterQuote", { usd: fmtUsd(r.reported) }) : ""].filter(Boolean).join(t("stats:usage.titleSep")) || undefined} data-price-source={by === "model" ? r.price_source ?? undefined : undefined}>{fmtUsd(r.est)}</span>}</td>
          <td className="py-2 pl-4"><Share r={share(r)} /></td>
        </tr>
      ))}
    </Table>
  )
}

/** label + number lines */
function Line({ l, v, sub, k }: { l: ReactNode; v: ReactNode; sub?: ReactNode; k: string }) {
  return (
    <div className="flex items-center gap-3 border-b py-2 last:border-0" data-line={k}>
      <span className="min-w-0 flex-1 type-callout">{l}{sub && <span className="ml-1.5 type-footnote text-muted-foreground">{sub}</span>}</span>
      <span className="w-20 text-right type-headline tabular-nums">{v}</span>
    </div>
  )
}

/** 耗时: how long finished items took, as a trend; where the time went; each kind of time side by side */
export function TimeSection({ s, f, fkey, live, range }: { s: Summary; f: Filter; fkey: string; live: boolean; range: string }) {
  const t = useT()
  const hourly = oneDay(f)
  const [lead] = useStats(() => stats.series(f, "lead", "none", hourly), [fkey], live)
  const [table, setTable] = useState(false)
  const tm = s.time
  const max = lead.s === "ok" ? Math.max(0, ...lead.v.series.flatMap((l) => l.values.map((v) => v ?? 0))) : 0
  const [unit, div] = durUnit(max)
  const fmt = (v: number) => { const x = v / div; return (x === 0 ? "0" : x < 10 ? x.toFixed(1).replace(/\.0$/, "") : Math.round(x)) + " " + unit }
  const d = lead.s === "ok" ? { ...lead.v, series: lead.v.series.map((l, i) => ({ ...l, label: i === 0 ? t("stats:time.median") : "p85" })) } : null
  const parts: [string, number, string][] = [[t("stats:time.agent"), tm.agent.sum, STATE.run], [t("stats:time.wait"), tm.wait.sum, STATE.wait], [t("stats:time.other"), tm.other.sum, STATE.idle]]
  const all = parts.reduce((a, p) => a + p[1], 0)
  const times = ([["lead", t("stats:time.lead"), tm.lead, t("stats:time.leadDef")], ["cycle", t("stats:time.cycle"), tm.cycle, t("stats:time.cycleDef")], ["first", t("stats:time.first"), tm.first_deliver, t("stats:time.firstDef")], ["wait", t("stats:time.wait"), tm.wait, t("stats:time.waitDef")], ["agent", t("stats:time.agent"), tm.agent, t("stats:time.agentDef")]] as const)
  return (
    <Section k="time" title={t("stats:time.title")} meta={t("stats:time.meta", { count: tm.lead.n, range })}>
      <div className="grid grid-cols-1 gap-10 md:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        <div data-lead-chart>
          <ChartHead k="lead" title={t("stats:time.leadTitle")} meta={hourly ? t("stats:time.leadMetaHourly", { unit, range, zone: zoneName(s.range.tz) }) : t("stats:time.leadMetaDaily", { unit, range })}
            legend={[{ label: t("stats:time.median"), c: SERIES[0], line: true }, { label: t("stats:time.medianToP85"), c: STATE.band }]} right={<ModeSwitch table={table} set={setTable} />} />
          {!d ? <Loading err={lead.s === "error" ? lead.msg : null} /> : table ? <ChartTable data={d} fmt={fmtMs} /> : <Chart data={d} kind="band" fmt={fmt} tipFmt={fmtMs} unit={div} colors={[SERIES[0], STATE.band]} label={hourly ? t("stats:time.leadAriaHourly") : t("stats:time.leadAriaDaily")} height={200} />}
          {d && d.series.every((l) => l.values.every((v) => v == null)) && <div className="mt-2 type-footnote text-muted-foreground">{t("stats:time.leadEmpty")}</div>}
          {tm.lead.n > 0 && <div className="mt-2 type-footnote text-muted-foreground" data-lead-sum>{t("stats:time.leadSum", { avg: fmtMs(tm.lead.avg), p50: fmtMs(tm.lead.p50), p85: fmtMs(tm.lead.p85) })}</div>}
        </div>
        <div className="flex flex-col gap-8">
          <div data-time-split>
            <ChartHead k="split" title={t("stats:time.split")} meta={t("stats:time.splitMeta")} />
            {all > 0 ? <>
              <div className="flex h-3 gap-[2px] overflow-hidden rounded-[4px]" role="img" aria-label={parts.map(([l, v]) => t("stats:time.splitPart", { label: l, pct: fmtPct(v / all) })).join(t("stats:word.commaSep"))}>
                {parts.map(([l, v, c]) => v > 0 && <span key={l} style={{ width: `${(v / all) * 100}%`, background: c }} />)}
              </div>
              <Legend className="mt-2" items={parts.map(([l, v, c]) => ({ label: t("stats:time.splitLegend", { label: l, pct: fmtPct(v / all), dur: fmtMs(v) }), c }))} />
            </> : <div className="type-callout text-muted-foreground">{t("stats:time.splitEmpty")}</div>}
          </div>
          <div>
            <ChartHead k="times" title={t("stats:time.times")} meta={t("stats:time.timesMeta")} />
            <HBars k="times" label={t("stats:time.timesAria")} fmt={fmtMs} vw="w-24" rows={times.map(([k, l, p, def]) => ({ key: k, label: l, title: def, value: p.p50 ?? 0, c: k === "wait" ? STATE.wait : k === "agent" ? STATE.run : STATE.created, sub: p.p85 != null ? `p85 ${fmtMs(p.p85)}` : "—" }))} />
          </div>
        </div>
      </div>
      {tm.releases + tm.not_released > 0 && <div className="type-footnote text-muted-foreground" data-releases>{[t("stats:time.releases", { count: tm.releases, pct: fmtPct(tm.release_success_rate) }), tm.not_released ? t("stats:time.notReleased", { count: tm.not_released }) : "", tm.accept_to_release.p50 != null ? t("stats:time.acceptToRelease", { dur: fmtMs(tm.accept_to_release.p50) }) : ""].filter(Boolean).join(" · ")}</div>}
    </Section>
  )
}

const FAIL_LABEL = (): Record<string, string> => ({ stall: t("stats:quality.fail.stall"), error: t("stats:quality.fail.error"), restart: t("stats:quality.fail.restart"), release: t("stats:quality.fail.release"), other: t("stats:quality.fail.other") })

/** 质量与介入 */
export function QualityHuman({ s, f, fkey, live, range, prevName }: { s: Summary; f: Filter; fkey: string; live: boolean; range: string; prevName: string }) {
  const t = useT()
  const [dk] = useStats(() => stats.counts(f, "decision_kind"), [fkey], live)
  const q = s.quality, h = s.human
  const fails = Object.entries(q.fails).filter(([, n]) => n > 0).sort((a, b) => b[1] - a[1])
  const failN = fails.reduce((a, [, n]) => a + n, 0), failLabel = FAIL_LABEL()
  return (
    <Section k="quality" title={t("stats:quality.title")} meta={range}>
      <div className="grid grid-cols-2 gap-x-6 gap-y-5 sm:grid-cols-4" data-quality-kpis>
        <Kpi small k="first_pass" label={t("stats:quality.firstPass")} value={fmtPct(q.first_pass)} foot={q.first_pass_n ? t("stats:quality.judged", { count: q.first_pass_n }) : t("stats:quality.noJudged")} def={t("stats:quality.firstPassDef")} />
        <Kpi small k="reworks" label={t("stats:quality.reworks")} value={q.reworks} foot={q.reworks_per_done != null ? t("stats:quality.reworksPerDone", { n: q.reworks_per_done.toFixed(1) }) : t("stats:quality.reworksPrev", { prev: prevName, n: s.prev?.quality.reworks ?? "—" })} def={t("stats:quality.reworksDef")} />
        <Kpi small k="turn_errors" label={t("stats:quality.turnErrors")} value={fmtPct(q.turn_error_rate, 1)} foot={t("stats:agent.turnsCount", { count: s.agents.turns, n: fmtN(s.agents.turns) })} def={t("stats:quality.turnErrorsDef")} />
        <Kpi small k="rollbacks" label={t("stats:quality.rollbacks")} value={q.rollbacks} foot={q.release_unknown ? t("stats:quality.releaseUnknown", { n: q.release_unknown }) : q.reopened ? t("stats:quality.reopened", { n: q.reopened }) : "　"} def={t("stats:quality.rollbacksDef")} />
      </div>
      <div className="grid grid-cols-1 gap-10 md:grid-cols-2">
        <div data-fails>
          <ChartHead k="fails" title={t("stats:quality.fails")} meta={t("stats:quality.failsMeta", { count: failN })} />
          {failN === 0 ? <div className="py-4 type-callout text-muted-foreground">{t("stats:quality.failsEmpty")}</div>
            : <HBars k="fails" label={t("stats:quality.failsAria")} rows={fails.map(([k, n]) => ({ key: k, label: failLabel[k] ?? k, value: n, c: STATE.fail }))} />}
        </div>
        <div data-human>
          <ChartHead k="decisions" title={t("stats:quality.decisions")} meta={t("stats:quality.decisionsMeta", { count: h.decisions })} />
          {dk.s !== "ok" ? <Loading h={96} err={dk.s === "loading" ? null : t("stats:quality.decisionsFailed")} /> : dk.v.rows.length === 0 ? <div className="py-4 type-callout text-muted-foreground">{t("stats:quality.decisionsEmpty")}</div>
            : <HBars k="decisions" label={t("stats:quality.decisionsAria")} rows={dk.v.rows.map((r: CountRow) => ({ key: r.key, label: r.label, value: r.count, c: STATE.wait, sub: r.response?.p50 != null ? fmtMs(r.response.p50) : "—", title: r.response?.p85 != null ? `p85 ${fmtMs(r.response.p85)}` : undefined }))} />}
          <div className="mt-3 type-footnote text-muted-foreground">
            {[
              t("stats:quality.response", { p50: fmtMs(h.response.p50), p85: fmtMs(h.response.p85) }),
              h.expired ? t("stats:quality.expired", { n: h.expired }) : "",
              t("stats:quality.messages", { n: h.messages }),
              h.per_done != null ? t("stats:quality.perDone", { n: h.per_done.toFixed(1) }) : "",
              h.yolo ? t("stats:quality.yolo", { decided: h.yolo.decided, declined: h.yolo.declined, undone: h.yolo.undone }) : "",
            ].filter(Boolean).join(" · ")}
          </div>
        </div>
      </div>
    </Section>
  )
}

const RED = ({ on }: { on: boolean }) => <span aria-hidden className={cn("inline-block size-2 shrink-0 rounded-full", on ? "bg-destructive" : "bg-tertiary/50")} />

/** Agent */
export function AgentSection({ s, f, fkey, live, range }: { s: Summary; f: Filter; fkey: string; live: boolean; range: string }) {
  const t = useT()
  const [tk] = useStats(() => stats.counts(f, "tool"), [fkey], live)
  const [open, setOpen] = useState(false)
  const a = s.agents
  const rows = tk.s === "ok" ? tk.v.rows : []
  const kinds = rows.filter((r) => r.dim !== "tool"), names = rows.filter((r) => r.dim === "tool").slice(0, 10)
  const total = kinds.reduce((x, r) => x + r.count, 0)
  return (
    <Section k="agents" title="Agent" meta={range}>
      <div className="grid grid-cols-1 gap-10 md:grid-cols-2">
        <div>
          <Line k="turns" l={t("stats:agent.turns")} sub={a.tokens_per_turn != null ? t("stats:agent.tokensPerTurn", { n: fmtN(a.tokens_per_turn) }) : undefined} v={fmtN(a.turns)} />
          <Line k="turn_ms" l={t("stats:agent.turnMs")} sub={`p85 ${fmtMs(a.turn_ms.p85)}`} v={fmtMs(a.turn_ms.p50)} />
          <Line k="tools" l={t("stats:agent.tools")} sub={a.tools_per_turn != null ? t("stats:agent.toolsPerTurn", { n: a.tools_per_turn.toFixed(1) }) : undefined} v={fmtN(a.tools)} />
          <Line k="tool_errors" l={<span className="inline-flex items-center gap-2"><RED on={(a.tool_error_rate ?? 0) > 0} />{t("stats:agent.toolErrors")}</span>} v={fmtPct(a.tool_error_rate, 1)} />
          <Line k="retries" l={t("stats:agent.retries")} sub={a.retry_success != null ? t("stats:agent.retrySuccess", { pct: fmtPct(a.retry_success) }) : undefined} v={a.retries} />
          <Line k="fallbacks" l={t("stats:agent.fallbacks")} sub={a.gateway_failed ? t("stats:agent.gatewayFailed", { n: a.gateway_failed }) : undefined} v={a.gateway_fallbacks} />
          <Line k="interrupted" l={t("stats:agent.interrupted")} sub={t("stats:agent.interruptedSub", { restart: a.interrupted, cancelled: a.cancelled })} v={a.interrupted + a.cancelled} />
          <Line k="permissions" l={t("stats:agent.permissions")} v={a.permissions} />
        </div>
        <div>
          <ChartHead k="tools" title={t("stats:agent.toolKinds")} meta={t("stats:agent.toolKindsMeta", { n: fmtN(total) })} />
          {!kinds.length ? <div className="py-4 type-callout text-muted-foreground">{t("stats:agent.toolKindsEmpty")}</div>
            : <HBars k="tools" label={t("stats:agent.toolKindsAria")} fmt={fmtN} rows={kinds.map((r) => ({ key: r.key, label: r.dim === "mcp" ? `Steve · ${r.label}` : r.label, title: r.label, value: r.count, c: SERIES[0], sub: total ? fmtPct(r.count / total) : undefined }))} />}
          {names.length > 0 && (
            <div className="mt-3">
              <button type="button" className="inline-flex items-center gap-1 type-footnote text-muted-foreground hover:text-foreground" onClick={() => setOpen(!open)} aria-expanded={open} data-tool-names>
                {open ? <ChevronDownIcon className="size-3.5" /> : <ChevronRightIcon className="size-3.5" />}{t("stats:agent.toolNames", { count: names.length })}
              </button>
              {open && <div className="mt-2">{names.map((r) => <div key={r.key} className="flex justify-between border-b py-1.5 type-callout last:border-0"><span className="font-mono text-[12.5px]">{r.label}</span><span className="tabular-nums">{r.count}</span></div>)}</div>}
            </div>
          )}
        </div>
      </div>
    </Section>
  )
}
