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
import { durUnit, fmtMs, fmtN, fmtPct, fmtUsd, useStats } from "@/stats/format"
import { harnessName } from "@/model-picker"

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

const USAGE_TABS = [["model", "模型"], ["bot", "Bot"], ["project", "项目"], ["purpose", "用途"]] as const
type UsageTab = (typeof USAGE_TABS)[number][0]

/** 用量明细: one table per dimension, or the item table */
export function UsageSection({ f, fkey, live, picked }: { f: Filter; fkey: string; live: boolean; picked: string | null }) {
  const [tab, setTab] = useState<UsageTab>("model")
  const g: Filter = picked ? { ...f, range: "custom", from: picked, to: picked } : f
  return (
    <section data-usage>
      <ChartHead k="usage-table" title="用量明细" meta={picked ? <span className="text-blue">只看 {picked}</span> : "按维度拆开，占比按估算费用"} right={<Segmented size="sm" value={tab} onChange={setTab} options={USAGE_TABS} />} />
      <UsageTable f={g} by={tab} fkey={fkey + picked} live={live} />
    </section>
  )
}

function UsageTable({ f, by, fkey, live }: { f: Filter; by: UsageTab; fkey: string; live: boolean }) {
  const [d] = useStats(() => stats.usage(f, by), [fkey, by], live)
  if (d.s === "loading") return <div className="flex h-24 items-center justify-center"><Spinner className="size-4 text-muted-foreground" /></div>
  if (d.s !== "ok") return <div className="py-6 type-callout text-muted-foreground" role="alert">{d.s === "error" ? d.msg : "读不到用量"}</div>
  const rows = d.v.rows.filter((r) => r.total > 0 || r.est > 0 || r.reported > 0 || r.turns > 0 || r.calls > 0)
  if (!rows.length) return <div className="py-6 type-callout text-muted-foreground" data-usage-empty>这段时间没有用量记录。</div>
  const est = rows.reduce((a, r) => a + r.est, 0), tok = rows.reduce((a, r) => a + r.total, 0)
  const share = (r: UsageRow) => (est > 0 ? r.est / est : tok > 0 ? r.total / tok : 0)
  return (
    <Table label="用量明细" min={720} head={<tr><th className={th}>名称</th><th className={cn(th, "pl-4 text-right")}>输入</th><th className={cn(th, "pl-4 text-right")}>输出</th><th className={cn(th, "pl-4 text-right")}>缓存读</th><th className={cn(th, "pl-4 text-right")}>缓存写</th><th className={cn(th, "pl-4 text-right")}>命中率</th><th className={cn(th, "pl-4 text-right")}><span className="inline-flex items-center gap-1">估算费用<Est /></span></th><th className={cn(th, "w-24 pl-4")}>占比</th></tr>}>
      {rows.map((r) => (
        <tr key={r.key} data-usage-row={r.key}>
          <td className="max-w-56 truncate py-2 pr-2" title={r.key}>{r.label}{by === "model" && r.key && !r.priced && <span className="ml-1 type-footnote text-muted-foreground">（未定价）</span>}
            {by === "purpose" && !!r.models?.length && <span className="block truncate text-[11.5px] text-muted-foreground" title={r.models.map((m) => `${m.harness ? harnessName(m.harness) + " · " : ""}${m.model} ×${m.calls}`).join("，")} data-purpose-models>{r.models.map((m, i) => <span key={i}>{i > 0 && " · "}{m.harness && (i === 0 || m.harness !== r.models![i - 1].harness) ? `${harnessName(m.harness)} ` : ""}<span className="font-mono">{m.model.replace(/^claude-/, "")}</span>{r.models!.length > 1 ? ` ×${m.calls}` : ""}</span>)}</span>}</td>
          <td className={num}>{fmtN(r.input)}</td><td className={num}>{fmtN(r.output)}</td><td className={num}>{fmtN(r.cache_read)}</td><td className={num}>{fmtN(r.cache_write)}</td>
          <td className={num}>{fmtPct(r.hit_rate)}</td>
          <td className={num}>{r.total === 0 && r.reported > 0 ? <span className="text-muted-foreground" title="只有会话级的适配器报价，是下限">报价 ≥{fmtUsd(r.reported)}</span> : by === "model" && r.key && !r.priced ? <a href="#/settings/stats" className="text-blue hover:underline" data-unpriced>未定价</a> : <span title={r.reported ? `适配器报价 ${fmtUsd(r.reported)}` : undefined}>{fmtUsd(r.est)}</span>}</td>
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
  const [lead] = useStats(() => stats.series(f, "lead", "none"), [fkey], live)
  const [table, setTable] = useState(false)
  const t = s.time
  const max = lead.s === "ok" ? Math.max(0, ...lead.v.series.flatMap((l) => l.values.map((v) => v ?? 0))) : 0
  const [unit, div] = durUnit(max)
  const fmt = (v: number) => { const x = v / div; return (x === 0 ? "0" : x < 10 ? x.toFixed(1).replace(/\.0$/, "") : Math.round(x)) + " " + unit }
  const d = lead.s === "ok" ? { ...lead.v, series: lead.v.series.map((l, i) => ({ ...l, label: i === 0 ? "中位数" : "p85" })) } : null
  const parts: [string, number, string][] = [["Agent 工作", t.agent.sum, STATE.run], ["等你", t.wait.sum, STATE.wait], ["其他", t.other.sum, STATE.idle]]
  const all = parts.reduce((a, p) => a + p[1], 0)
  const times = ([["lead", "前置时间", t.lead, "新建到完成"], ["cycle", "周期时间", t.cycle, "第一个回合到被接受的交付"], ["first", "首次交付", t.first_deliver, "第一个回合到第一次交付"], ["wait", "等你", t.wait, "需要你处理的卡片挂着的时间"], ["agent", "Agent 工作", t.agent, "回合时间的并集"]] as const)
  return (
    <Section k="time" title="耗时" meta={`区间内完成的 ${t.lead.n} 件事项 · ${range}`}>
      <div className="grid grid-cols-1 gap-10 md:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        <div data-lead-chart>
          <ChartHead k="lead" title="完成耗时（前置时间）" meta={`${unit} · 每天完成的事项，线是中位数 · ${range}`}
            legend={[{ label: "中位数", c: SERIES[0], line: true }, { label: "中位数到 p85", c: STATE.band }]} right={<ModeSwitch table={table} set={setTable} />} />
          {!d ? <Loading err={lead.s === "error" ? lead.msg : null} /> : table ? <ChartTable data={d} fmt={fmtMs} /> : <Chart data={d} kind="band" fmt={fmt} tipFmt={fmtMs} unit={div} colors={[SERIES[0], STATE.band]} label="每天完成事项的前置时间" height={200} />}
          {d && d.series.every((l) => l.values.every((v) => v == null)) && <div className="mt-2 type-footnote text-muted-foreground">这段时间还没有完成的事项。</div>}
          {t.lead.n > 0 && <div className="mt-2 type-footnote text-muted-foreground" data-lead-sum>平均 {fmtMs(t.lead.avg)} · 中位 {fmtMs(t.lead.p50)} · p85 {fmtMs(t.lead.p85)}</div>}
        </div>
        <div className="flex flex-col gap-8">
          <div data-time-split>
            <ChartHead k="split" title="时间花在哪" meta="完成事项合计" />
            {all > 0 ? <>
              <div className="flex h-3 gap-[2px] overflow-hidden rounded-[4px]" role="img" aria-label={parts.map(([l, v]) => `${l} ${fmtPct(v / all)}`).join("，")}>
                {parts.map(([l, v, c]) => v > 0 && <span key={l} style={{ width: `${(v / all) * 100}%`, background: c }} />)}
              </div>
              <Legend className="mt-2" items={parts.map(([l, v, c]) => ({ label: `${l} ${fmtPct(v / all)} · ${fmtMs(v)}`, c }))} />
            </> : <div className="type-callout text-muted-foreground">还没有完成的事项。</div>}
          </div>
          <div>
            <ChartHead k="times" title="各段耗时" meta="中位数；下面灰字是 p85" />
            <HBars k="times" label="各段耗时的中位数" fmt={fmtMs} vw="w-24" rows={times.map(([k, l, p, def]) => ({ key: k, label: l, title: def, value: p.p50 ?? 0, c: k === "wait" ? STATE.wait : k === "agent" ? STATE.run : STATE.created, sub: p.p85 != null ? `p85 ${fmtMs(p.p85)}` : "—" }))} />
          </div>
        </div>
      </div>
      {t.releases + t.not_released > 0 && <div className="type-footnote text-muted-foreground" data-releases>发布 {t.releases} 次，成功率 {fmtPct(t.release_success_rate)}{t.not_released ? ` · 未开始就被拒 ${t.not_released} 次` : ""}{t.accept_to_release.p50 != null ? ` · 验收到上线中位 ${fmtMs(t.accept_to_release.p50)}` : ""}</div>}
    </Section>
  )
}

const FAIL_LABEL: Record<string, string> = { stall: "停滞", error: "会话出错", restart: "Hub 重启", release: "发布失败", other: "其他" }

/** 质量与介入 */
export function QualityHuman({ s, f, fkey, live, range, prevName }: { s: Summary; f: Filter; fkey: string; live: boolean; range: string; prevName: string }) {
  const [dk] = useStats(() => stats.counts(f, "decision_kind"), [fkey], live)
  const q = s.quality, h = s.human
  const fails = Object.entries(q.fails).filter(([, n]) => n > 0).sort((a, b) => b[1] - a[1])
  const failN = fails.reduce((a, [, n]) => a + n, 0)
  return (
    <Section k="quality" title="质量与介入" meta={range}>
      <div className="grid grid-cols-2 gap-x-6 gap-y-5 sm:grid-cols-4" data-quality-kpis>
        <Kpi small k="first_pass" label="首次通过率" value={fmtPct(q.first_pass)} foot={q.first_pass_n ? `${q.first_pass_n} 件已裁决` : "还没有裁决的交付"} def="第一次交付就验收通过（人工通过或自动验收）的事项比例；被新交付取代的那次不算裁决。" />
        <Kpi small k="reworks" label="返工" value={q.reworks} foot={q.reworks_per_done != null ? `每件完成事项 ${q.reworks_per_done.toFixed(1)} 次` : `${prevName} ${s.prev?.quality.reworks ?? "—"} 次`} def="交付被打回、重新做的次数。" />
        <Kpi small k="turn_errors" label="回合出错率" value={fmtPct(q.turn_error_rate, 1)} foot={`${fmtN(s.agents.turns)} 个回合`} def="出错结束的 Agent 回合 ÷ 全部回合。" />
        <Kpi small k="rollbacks" label="回滚" value={q.rollbacks} foot={q.release_unknown ? `不知道现在版本 ${q.release_unknown}` : q.reopened ? `重开 ${q.reopened}` : "　"} def="发布后被回滚的次数；“重开”是完成后又重新打开的事项。" />
      </div>
      <div className="grid grid-cols-1 gap-10 md:grid-cols-2">
        <div data-fails>
          <ChartHead k="fails" title="失败卡 · 按原因" meta={`张 · 共 ${failN} 张`} />
          {failN === 0 ? <div className="py-4 type-callout text-muted-foreground">这段时间没有失败卡。</div>
            : <HBars k="fails" label="失败卡按原因" rows={fails.map(([k, n]) => ({ key: k, label: FAIL_LABEL[k] ?? k, value: n, c: STATE.fail }))} />}
        </div>
        <div data-human>
          <ChartHead k="decisions" title="需要你决定的事 · 按种类" meta={`个 · 共 ${h.decisions} 个；数字下面是你的响应中位时间`} />
          {dk.s !== "ok" ? <Loading h={96} err={dk.s === "loading" ? null : "读不到决定"} /> : dk.v.rows.length === 0 ? <div className="py-4 type-callout text-muted-foreground">这段时间没有需要你决定的事。</div>
            : <HBars k="decisions" label="决定按种类" rows={dk.v.rows.map((r: CountRow) => ({ key: r.key, label: r.label, value: r.count, c: STATE.wait, sub: r.response?.p50 != null ? fmtMs(r.response.p50) : "—", title: r.response?.p85 != null ? `p85 ${fmtMs(r.response.p85)}` : undefined }))} />}
          <div className="mt-3 type-footnote text-muted-foreground">
            响应中位 {fmtMs(h.response.p50)} · p85 {fmtMs(h.response.p85)}{h.expired ? ` · 过期或作废 ${h.expired}` : ""} · 你补充的消息 {h.messages}{h.per_done != null ? ` · 每件完成事项介入 ${h.per_done.toFixed(1)} 次` : ""}
            {h.yolo && ` · YOLO 代办 ${h.yolo.decided}，留给你 ${h.yolo.declined}，撤销 ${h.yolo.undone}`}
          </div>
        </div>
      </div>
    </Section>
  )
}

const RED = ({ on }: { on: boolean }) => <span aria-hidden className={cn("inline-block size-2 shrink-0 rounded-full", on ? "bg-destructive" : "bg-tertiary/50")} />

/** Agent */
export function AgentSection({ s, f, fkey, live, range }: { s: Summary; f: Filter; fkey: string; live: boolean; range: string }) {
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
          <Line k="turns" l="回合" sub={a.tokens_per_turn != null ? `每回合 ${fmtN(a.tokens_per_turn)} tokens` : undefined} v={fmtN(a.turns)} />
          <Line k="turn_ms" l="回合时长 · 中位" sub={`p85 ${fmtMs(a.turn_ms.p85)}`} v={fmtMs(a.turn_ms.p50)} />
          <Line k="tools" l="工具调用" sub={a.tools_per_turn != null ? `每回合 ${a.tools_per_turn.toFixed(1)} 次` : undefined} v={fmtN(a.tools)} />
          <Line k="tool_errors" l={<span className="inline-flex items-center gap-2"><RED on={(a.tool_error_rate ?? 0) > 0} />工具失败率</span>} v={fmtPct(a.tool_error_rate, 1)} />
          <Line k="retries" l="网关 503 重试" sub={a.retry_success != null ? `重试后成功 ${fmtPct(a.retry_success)}` : undefined} v={a.retries} />
          <Line k="fallbacks" l="网关回退" sub={a.gateway_failed ? `失败 ${a.gateway_failed}` : undefined} v={a.gateway_fallbacks} />
          <Line k="interrupted" l="中断" sub={`Hub 重启 ${a.interrupted} · 你中断 ${a.cancelled}`} v={a.interrupted + a.cancelled} />
          <Line k="permissions" l="权限请求" v={a.permissions} />
        </div>
        <div>
          <ChartHead k="tools" title="工具调用 · 按种类" meta={`次 · 共 ${fmtN(total)} 次`} />
          {!kinds.length ? <div className="py-4 type-callout text-muted-foreground">这段时间没有工具调用。</div>
            : <HBars k="tools" label="工具调用按种类" fmt={fmtN} rows={kinds.map((r) => ({ key: r.key, label: r.dim === "mcp" ? `Steve · ${r.label}` : r.label, title: r.label, value: r.count, c: SERIES[0], sub: total ? fmtPct(r.count / total) : undefined }))} />}
          {names.length > 0 && (
            <div className="mt-3">
              <button type="button" className="inline-flex items-center gap-1 type-footnote text-muted-foreground hover:text-foreground" onClick={() => setOpen(!open)} aria-expanded={open} data-tool-names>
                {open ? <ChevronDownIcon className="size-3.5" /> : <ChevronRightIcon className="size-3.5" />}按工具名 Top {names.length}（与上图是同一批调用）
              </button>
              {open && <div className="mt-2">{names.map((r) => <div key={r.key} className="flex justify-between border-b py-1.5 type-callout last:border-0"><span className="font-mono text-[12.5px]">{r.label}</span><span className="tabular-nums">{r.count}</span></div>)}</div>}
            </div>
          )}
        </div>
      </div>
    </Section>
  )
}
