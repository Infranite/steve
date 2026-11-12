/* 统计页的分节（stats.md §6.3 第 4–7 条，ST-40 重整）：耗时、用量明细、质量与介入、Agent。
 * 每一节是一组面板（panel.tsx）：头部讲这张图是什么，正文是图，数字条给这一节的几个数，脚注放说明；分布用横向条形（HBars），趋势用图。
 * 颜色同概览：完成绿、等你橙、失败红、Agent 工作蓝，其余灰；“谁”的分组用 SERIES。 */
import { useState, type ReactNode } from "react"
import { cn } from "cn"
import { BotIcon, ChevronDownIcon, ChevronRightIcon, ShieldCheckIcon, TableIcon, TimerIcon } from "lucide-react"
import { stats, type CountRow, type Filter, type Summary, type UsageRow } from "@/api/stats"
import { Segmented } from "@/ui"
import { Spinner } from "@/components/ui/spinner"
import { ChartBody, Chart, ChartHead, ChartTable, HBars, Legend, Share, STATE, SERIES } from "@/stats/chart"
import { Est, Kpi, kpiRow } from "@/stats/kpi"
import { Loading, ModeSwitch } from "@/stats/overview"
import { bucketName, durUnit, fmtMs, fmtN, fmtPct, fmtUsd, isHour, oneDay, zoneName, useStats } from "@/stats/format"
import { Panel, PanelFoot, PanelSection } from "@/panel"
import { harnessName } from "@/model-picker"
import { t, useT } from "@/i18n"

export const th = "whitespace-nowrap py-2 font-medium text-muted-foreground"
export const num = "whitespace-nowrap py-2 pl-4 text-right font-mono text-[12.5px] tabular-nums"

/** a part of the page: its panels, under a heading only screen readers need (each panel names itself and its range) */
export function Section({ title, meta, children, k }: { title: ReactNode; meta?: ReactNode; children: ReactNode; k: string }) {
  return (
    <section className="flex flex-col gap-4" data-section={k}>
      <h2 className="sr-only">{title}{meta ? <> · {meta}</> : null}</h2>
      {children}
    </section>
  )
}

/** a hairline table inside a panel, scrolling sideways on a phone */
export function Table({ head, children, label, min = 560 }: { head: ReactNode; children: ReactNode; label: string; min?: number }) {
  return (
    <div className="overflow-x-auto px-5">
      <table className="w-full type-callout" style={{ minWidth: min }} aria-label={label}>
        <thead className="border-b text-left type-footnote">{head}</thead>
        <tbody className="[&>tr]:border-b [&>tr]:border-border/60 [&>tr:last-child]:border-0">{children}</tbody>
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
    <Panel k="usage-table" data-usage>
      <ChartHead k="usage-table" icon={<TableIcon />} title={t("stats:usage.title")} sub={t("stats:usage.meta")} meta={picked ? <span className="text-blue">{t("stats:usage.only", { bucket: bucketName(picked) })}</span> : undefined} right={<Segmented size="sm" value={tab} onChange={setTab} options={USAGE_TABS()} />} />
      <ChartBody className="px-0 pt-1 pb-2"><UsageTable f={g} by={tab} fkey={fkey + picked} live={live} /></ChartBody>
    </Panel>
  )
}

function UsageTable({ f, by, fkey, live }: { f: Filter; by: UsageTab; fkey: string; live: boolean }) {
  const t = useT()
  const [d] = useStats(() => stats.usage(f, by), [fkey, by], live)
  if (d.s === "loading") return <div className="flex h-24 items-center justify-center"><Spinner className="size-4 text-muted-foreground" /></div>
  if (d.s !== "ok") return <div className="px-5 py-6 type-callout text-muted-foreground" role="alert">{d.s === "error" ? d.msg : t("stats:usage.loadFailed")}</div>
  const rows = d.v.rows.filter((r) => r.total > 0 || r.est > 0 || r.reported > 0 || r.turns > 0 || r.calls > 0)
  if (!rows.length) return <div className="px-5 py-6 type-callout text-muted-foreground" data-usage-empty>{t("stats:usage.empty")}</div>
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
  const releases = tm.releases + tm.not_released > 0 && [t("stats:time.releases", { count: tm.releases, pct: fmtPct(tm.release_success_rate) }), tm.not_released ? t("stats:time.notReleased", { count: tm.not_released }) : "", tm.accept_to_release.p50 != null ? t("stats:time.acceptToRelease", { dur: fmtMs(tm.accept_to_release.p50) }) : ""].filter(Boolean).join(" · ")
  return (
    <Section k="time" title={t("stats:time.title")} meta={t("stats:time.meta", { count: tm.lead.n, range })}>
      <div className="grid grid-cols-1 gap-4 @4xl/stats:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        <Panel k="lead" className="@container" data-lead-chart>
          <ChartHead k="lead" icon={<TimerIcon />} title={t("stats:time.leadTitle")} sub={t("stats:panel.leadSub")} meta={hourly ? t("stats:time.leadMetaHourly", { unit, range, zone: zoneName(s.range.tz) }) : t("stats:time.leadMetaDaily", { unit, range })}
            legend={[{ label: t("stats:time.median"), c: SERIES[0], line: true }, { label: t("stats:time.medianToP85"), c: STATE.band }]} right={<ModeSwitch table={table} set={setTable} />} />
          <ChartBody>
            {!d ? <Loading err={lead.s === "error" ? lead.msg : null} /> : table ? <ChartTable data={d} fmt={fmtMs} /> : <Chart data={d} kind="band" fmt={fmt} tipFmt={fmtMs} unit={div} colors={[SERIES[0], STATE.band]} label={hourly ? t("stats:time.leadAriaHourly") : t("stats:time.leadAriaDaily")} height={200} />}
            {d && d.series.every((l) => l.values.every((v) => v == null)) && <div className="mt-2 type-footnote text-muted-foreground">{t("stats:time.leadEmpty")}</div>}
          </ChartBody>
          <div className={kpiRow(4)} data-lead-sum>
            <Kpi k="lead_p50" label={t("stats:time.median")} value={fmtMs(tm.lead.p50)} def={t("stats:time.leadDef")} />
            <Kpi k="lead_p85" label="p85" value={fmtMs(tm.lead.p85)} def={t("stats:time.leadDef")} />
            <Kpi k="lead_avg" label={t("stats:panel.avg")} value={fmtMs(tm.lead.avg)} def={t("stats:time.leadDef")} />
            <Kpi k="lead_n" label={t("stats:panel.doneItems")} value={tm.lead.n} def={t("stats:time.leadDef")} />
          </div>
        </Panel>
        <Panel k="split" icon={<ClockSplit />} title={t("stats:time.split")} sub={t("stats:time.splitMeta")}>
          <PanelSection k="split" label={t("stats:panel.share")} meta={all > 0 ? fmtMs(all) : undefined}>
            <div data-time-split>
              {all > 0 ? <>
                <div className="flex h-2.5 gap-[2px] overflow-hidden rounded-full" role="img" aria-label={parts.map(([l, v]) => t("stats:time.splitPart", { label: l, pct: fmtPct(v / all) })).join(t("stats:word.commaSep"))}>
                  {parts.map(([l, v, c]) => v > 0 && <span key={l} style={{ width: `${(v / all) * 100}%`, background: c }} />)}
                </div>
                <Legend className="mt-2.5" items={parts.map(([l, v, c]) => ({ label: t("stats:time.splitLegend", { label: l, pct: fmtPct(v / all), dur: fmtMs(v) }), c }))} />
              </> : <div className="type-callout text-muted-foreground">{t("stats:time.splitEmpty")}</div>}
            </div>
          </PanelSection>
          <PanelSection k="times" label={t("stats:time.times")} meta={t("stats:time.timesMeta")}>
            <div className="-mx-1.5"><HBars k="times" label={t("stats:time.timesAria")} fmt={fmtMs} vw="w-32" rows={times.map(([k, l, p, def]) => ({ key: k, label: l, title: def, value: p.p50 ?? 0, c: k === "wait" ? STATE.wait : k === "agent" ? STATE.run : STATE.created, sub: p.p85 != null ? `p85 ${fmtMs(p.p85)}` : "—" }))} /></div>
          </PanelSection>
          {releases && <PanelFoot meta={<span data-releases>{releases}</span>} />}
        </Panel>
      </div>
    </Section>
  )
}
const ClockSplit = () => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round"><circle cx="12" cy="12" r="9" /><path d="M12 3v9l6.4 6.4" /></svg>

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
      <Panel k="quality" className="@container" icon={<ShieldCheckIcon />} title={t("stats:quality.title")} sub={t("stats:panel.qualitySub")}>
        <div className="grid grid-cols-1 @2xl:grid-cols-2 @2xl:[&>*+*]:border-l @max-2xl:[&>*+*]:border-t">
          <PanelSection k="fails" label={t("stats:quality.fails")} meta={t("stats:quality.failsMeta", { count: failN })}>
            <div data-fails>
              {failN === 0 ? <div className="py-4 type-callout text-muted-foreground">{t("stats:quality.failsEmpty")}</div>
                : <div className="-mx-1.5"><HBars k="fails" label={t("stats:quality.failsAria")} rows={fails.map(([k, n]) => ({ key: k, label: failLabel[k] ?? k, value: n, c: STATE.fail }))} /></div>}
            </div>
          </PanelSection>
          <PanelSection k="decisions" label={t("stats:quality.decisions")} meta={t("stats:quality.decisionsMeta", { count: h.decisions })}>
            <div data-human>
              {dk.s !== "ok" ? <Loading h={96} err={dk.s === "loading" ? null : t("stats:quality.decisionsFailed")} /> : dk.v.rows.length === 0 ? <div className="py-4 type-callout text-muted-foreground">{t("stats:quality.decisionsEmpty")}</div>
                : <div className="-mx-1.5"><HBars k="decisions" vw="w-28" label={t("stats:quality.decisionsAria")} rows={dk.v.rows.map((r: CountRow) => ({ key: r.key, label: r.label, value: r.count, c: STATE.wait, sub: r.response?.p50 != null ? fmtMs(r.response.p50) : "—", title: r.response?.p85 != null ? `p85 ${fmtMs(r.response.p85)}` : undefined }))} /></div>}
            </div>
          </PanelSection>
        </div>
        <div className={kpiRow(4)} data-quality-kpis>
          <Kpi k="first_pass" label={t("stats:quality.firstPass")} value={fmtPct(q.first_pass)} foot={q.first_pass_n ? t("stats:quality.judged", { count: q.first_pass_n }) : t("stats:quality.noJudged")} def={t("stats:quality.firstPassDef")} />
          <Kpi k="reworks" label={t("stats:quality.reworks")} value={q.reworks} tone={q.reworks ? "wait" : undefined} foot={q.reworks_per_done != null ? t("stats:quality.reworksPerDone", { n: q.reworks_per_done.toFixed(1) }) : t("stats:quality.reworksPrev", { prev: prevName, n: s.prev?.quality.reworks ?? "—" })} def={t("stats:quality.reworksDef")} />
          <Kpi k="turn_errors" label={t("stats:quality.turnErrors")} value={fmtPct(q.turn_error_rate, 1)} foot={t("stats:agent.turnsCount", { count: s.agents.turns, n: fmtN(s.agents.turns) })} def={t("stats:quality.turnErrorsDef")} />
          <Kpi k="rollbacks" label={t("stats:quality.rollbacks")} value={q.rollbacks} tone={q.rollbacks ? "bad" : undefined} foot={q.release_unknown ? t("stats:quality.releaseUnknown", { n: q.release_unknown }) : q.reopened ? t("stats:quality.reopened", { n: q.reopened }) : "　"} def={t("stats:quality.rollbacksDef")} />
        </div>
        <PanelFoot meta={[
          t("stats:quality.response", { p50: fmtMs(h.response.p50), p85: fmtMs(h.response.p85) }),
          h.expired ? t("stats:quality.expired", { n: h.expired }) : "",
          t("stats:quality.messages", { n: h.messages }),
          h.per_done != null ? t("stats:quality.perDone", { n: h.per_done.toFixed(1) }) : "",
          h.yolo ? t("stats:quality.yolo", { decided: h.yolo.decided, declined: h.yolo.declined, undone: h.yolo.undone }) : "",
        ].filter(Boolean).join(" · ")} />
      </Panel>
    </Section>
  )
}

/** Agent */
export function AgentSection({ s, f, fkey, live, range }: { s: Summary; f: Filter; fkey: string; live: boolean; range: string }) {
  const t = useT()
  const [tk] = useStats(() => stats.counts(f, "tool"), [fkey], live)
  const [open, setOpen] = useState(false)
  const [all, setAll] = useState(false)
  const a = s.agents
  const rows = tk.s === "ok" ? tk.v.rows : []
  const kinds = rows.filter((r) => r.dim !== "tool"), names = rows.filter((r) => r.dim === "tool").slice(0, 10)
  const total = kinds.reduce((x, r) => x + r.count, 0)
  const shown = all ? kinds : kinds.slice(0, 10)
  return (
    <Section k="agents" title="Agent" meta={range}>
      <Panel k="agents" className="@container" icon={<BotIcon />} title="Agent" sub={t("stats:panel.agentSub")}>
        <div className={kpiRow(8)} data-agent-lines>
          <Kpi k="turns" label={t("stats:agent.turns")} value={fmtN(a.turns)} foot={a.tokens_per_turn != null ? t("stats:agent.tokensPerTurn", { n: fmtN(a.tokens_per_turn) }) : undefined} def={t("stats:agent.turns")} />
          <Kpi k="turn_ms" label={t("stats:agent.turnMs")} value={fmtMs(a.turn_ms.p50)} foot={`p85 ${fmtMs(a.turn_ms.p85)}`} def={t("stats:agent.turnMs")} />
          <Kpi k="tools" label={t("stats:agent.tools")} value={fmtN(a.tools)} foot={a.tools_per_turn != null ? t("stats:agent.toolsPerTurn", { n: a.tools_per_turn.toFixed(1) }) : undefined} def={t("stats:agent.tools")} />
          <Kpi k="tool_errors" label={t("stats:agent.toolErrors")} value={fmtPct(a.tool_error_rate, 1)} tone={(a.tool_error_rate ?? 0) > 0 ? "bad" : undefined} def={t("stats:agent.toolErrors")} />
          <Kpi k="retries" label={t("stats:agent.retries")} value={a.retries} foot={a.retry_success != null ? t("stats:agent.retrySuccess", { pct: fmtPct(a.retry_success) }) : undefined} def={t("stats:agent.retries")} />
          <Kpi k="fallbacks" label={t("stats:agent.fallbacks")} value={a.gateway_fallbacks} foot={a.gateway_failed ? t("stats:agent.gatewayFailed", { n: a.gateway_failed }) : undefined} def={t("stats:agent.fallbacks")} />
          <Kpi k="interrupted" label={t("stats:agent.interrupted")} value={a.interrupted + a.cancelled} foot={t("stats:agent.interruptedSub", { restart: a.interrupted, cancelled: a.cancelled })} def={t("stats:agent.interrupted")} />
          <Kpi k="permissions" label={t("stats:agent.permissions")} value={a.permissions} def={t("stats:agent.permissions")} />
        </div>
        <PanelSection k="tools" label={t("stats:agent.toolKinds")} meta={t("stats:agent.toolKindsMeta", { n: fmtN(total) })}>
          {!kinds.length ? <div className="py-4 type-callout text-muted-foreground">{t("stats:agent.toolKindsEmpty")}</div>
            : <div className="-mx-1.5 grid grid-cols-1 gap-x-10 @3xl/stats:grid-cols-2">{[shown.slice(0, Math.ceil(shown.length / 2)), shown.slice(Math.ceil(shown.length / 2))].map((half, i) => half.length > 0 &&
              <HBars key={i} k="tools" lw="w-32 sm:w-48" label={t("stats:agent.toolKindsAria")} fmt={fmtN} total={kinds[0]?.count} rows={half.map((r) => ({ key: r.key, label: r.dim === "mcp" ? `Steve · ${r.label}` : r.label, title: r.dim === "mcp" ? `Steve · ${r.label}` : r.label, value: r.count, c: SERIES[0], sub: total ? fmtPct(r.count / total) : undefined }))} />)}</div>}
          {kinds.length > shown.length && <button type="button" className="mt-1 type-footnote text-muted-foreground hover:text-foreground" onClick={() => setAll(true)} data-tool-kinds-more>{t("stats:overview.showMore", { count: kinds.length - shown.length })}</button>}
        </PanelSection>
        {names.length > 0 && (
          <PanelSection k="tool-names">
            <button type="button" className="inline-flex items-center gap-1 type-footnote text-muted-foreground hover:text-foreground" onClick={() => setOpen(!open)} aria-expanded={open} data-tool-names>
              {open ? <ChevronDownIcon className="size-3.5" /> : <ChevronRightIcon className="size-3.5" />}{t("stats:agent.toolNames", { count: names.length })}
            </button>
            {open && <div className="mt-2">{names.map((r) => <div key={r.key} className="flex justify-between border-b border-border/60 py-1.5 type-callout last:border-0"><span className="font-mono text-[12.5px]">{r.label}</span><span className="font-mono text-[12.5px] tabular-nums">{r.count}</span></div>)}</div>}
          </PanelSection>
        )}
      </Panel>
    </Section>
  )
}
