/* 统计页的明细（stats.md §6.3 第 4–7 条）：用量明细、流程与时间、质量与介入、Agent。
 * 不加卡片框，靠字号、留白和细分隔线分层；数字 tabular-nums；红色只用于失败、回滚、出错的状态点。 */
import { useState, type ReactNode } from "react"
import { cn } from "cn"
import { ChevronDownIcon, ChevronRightIcon } from "lucide-react"
import { stats, type CountRow, type Filter, type Summary, type UsageRow } from "@/api/stats"
import { Segmented, SectionLabel } from "@/ui"
import { Spinner } from "@/components/ui/spinner"
import { Share, Spark } from "@/stats/chart"
import { Est } from "@/stats/kpi"
import { ItemsTable } from "@/stats/items"
import { fmtMs, fmtN, fmtPct, fmtUsd, useStats } from "@/stats/format"
import { harnessName } from "@/model-picker"

export const th = "py-2 font-medium text-muted-foreground"
export const num = "py-2 pl-4 text-right tabular-nums"

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

const USAGE_TABS = [["model", "模型"], ["bot", "Bot"], ["project", "项目"], ["purpose", "用途"], ["items", "事项"]] as const
type UsageTab = (typeof USAGE_TABS)[number][0]

/** 用量明细: one table per dimension, or the item table */
export function UsageSection({ f, fkey, live, picked }: { f: Filter; fkey: string; live: boolean; picked: string | null }) {
  const [tab, setTab] = useState<UsageTab>("model")
  const g: Filter = picked ? { ...f, range: "custom", from: picked, to: picked } : f
  return (
    <section data-usage>
      <SectionLabel right={picked ? <span className="type-footnote text-blue">只看 {picked}</span> : undefined}>用量明细</SectionLabel>
      <Segmented size="sm" className="mb-3" value={tab} onChange={setTab} options={USAGE_TABS} />
      {tab === "items" ? <ItemsTable f={g} fkey={fkey + picked} live={live} /> : <UsageTable f={g} by={tab} fkey={fkey + picked} live={live} />}
    </section>
  )
}

function UsageTable({ f, by, fkey, live }: { f: Filter; by: Exclude<UsageTab, "items">; fkey: string; live: boolean }) {
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
function Line({ l, v, sub, spark, k }: { l: ReactNode; v: ReactNode; sub?: ReactNode; spark?: number[]; k: string }) {
  return (
    <div className="flex items-center gap-3 border-b py-2.5 last:border-0" data-line={k}>
      <span className="min-w-0 flex-1 type-callout">{l}{sub && <span className="ml-1.5 type-footnote text-muted-foreground">{sub}</span>}</span>
      {spark && <Spark values={spark} />}
      <span className="w-20 text-right type-headline tabular-nums">{v}</span>
    </div>
  )
}

/** 流程与时间: throughput on the left, times on the right, and where the time went */
export function FlowTime({ s, f, fkey, live }: { s: Summary; f: Filter; fkey: string; live: boolean }) {
  const [cr] = useStats(() => stats.series(f, "created", "none"), [fkey], live)
  const [dn] = useStats(() => stats.series(f, "done", "none"), [fkey], live)
  const [wp] = useStats(() => stats.series(f, "wip", "none"), [fkey], live)
  const sp = (x: typeof cr) => (x.s === "ok" && x.v.buckets.length > 1 ? (x.v.series[0]?.values ?? []).map((v) => v ?? 0) : undefined)
  const t = s.time, th_ = s.throughput
  const parts: [string, number, string][] = [["Agent 工作", t.agent.sum, "bg-foreground/60"], ["等你", t.wait.sum, "bg-foreground/30"], ["其他", t.other.sum, "bg-foreground/12"]]
  const all = parts.reduce((a, p) => a + p[1], 0)
  return (
    <section className="grid gap-10 md:grid-cols-2" data-flow>
      <div>
        <SectionLabel>吞吐</SectionLabel>
        <Line k="created" l="新建" v={th_.created} spark={sp(cr)} />
        <Line k="done" l="完成" v={th_.done} spark={sp(dn)} />
        <Line k="dropped" l="放弃" sub={th_.drop_rate != null ? `放弃率 ${fmtPct(th_.drop_rate)}` : undefined} v={th_.dropped} />
        <Line k="delivered" l="交付" sub={th_.delivered ? `自动验收 ${th_.delivered_auto} · 人工 ${th_.delivered_manual}` : undefined} v={th_.delivered} />
        <Line k="wip" l="当前进行中" v={th_.wip} spark={sp(wp)} />
      </div>
      <div>
        <SectionLabel>时间 <span className="font-normal">中位数 / p85</span></SectionLabel>
        {([["前置时间", t.lead, "新建到完成"], ["周期时间", t.cycle, "第一个回合到被接受的交付"], ["首次交付", t.first_deliver, "第一个回合到第一次交付"], ["等你", t.wait, "需要你处理的卡片挂着的时间"], ["Agent 工作", t.agent, "回合时间的并集"]] as const).map(([l, p, d]) => (
          <div key={l} className="flex items-center gap-3 border-b py-2.5 last:border-0" title={d} data-time={l}>
            <span className="min-w-0 flex-1 type-callout">{l}</span>
            <span className="type-headline tabular-nums">{fmtMs(p.p50)}</span><span className="w-24 text-right type-footnote tabular-nums text-muted-foreground">{fmtMs(p.p85)}</span>
          </div>
        ))}
        {all > 0 && (
          <div className="mt-4" data-time-split>
            <div className="mb-1.5 type-footnote text-muted-foreground">时间都花在哪（完成事项合计）</div>
            <div className="flex h-2.5 gap-[2px] overflow-hidden rounded-full" role="img" aria-label={parts.map(([l, v]) => `${l} ${fmtPct(v / all)}`).join("，")}>
              {parts.map(([l, v, c]) => v > 0 && <span key={l} className={c} style={{ width: `${(v / all) * 100}%` }} />)}
            </div>
            <div className="mt-1.5 flex flex-wrap gap-x-4 gap-y-1 type-footnote text-muted-foreground">
              {parts.map(([l, v, c]) => <span key={l} className="inline-flex items-center gap-1.5"><span className={cn("size-2.5 rounded-[3px]", c)} />{l} {fmtPct(v / all)} · {fmtMs(v)}</span>)}
            </div>
          </div>
        )}
        {t.releases + t.not_released > 0 && <div className="mt-4 type-footnote text-muted-foreground" data-releases>发布 {t.releases} 次，成功率 {fmtPct(t.release_success_rate)}{t.not_released ? ` · 未开始就被拒 ${t.not_released} 次` : ""}{t.accept_to_release.p50 != null ? ` · 验收到上线中位 ${fmtMs(t.accept_to_release.p50)}` : ""}</div>}
      </div>
    </section>
  )
}

const RED = ({ on }: { on: boolean }) => <span aria-hidden className={cn("inline-block size-2 shrink-0 rounded-full", on ? "bg-destructive" : "bg-tertiary/50")} />
const FAIL_LABEL: Record<string, string> = { stall: "停滞", error: "会话出错", restart: "Hub 重启", release: "发布失败", other: "其他" }

/** 质量与介入 */
export function QualityHuman({ s, f, fkey, live }: { s: Summary; f: Filter; fkey: string; live: boolean }) {
  const [dk] = useStats(() => stats.counts(f, "decision_kind"), [fkey], live)
  const q = s.quality, h = s.human
  const fails = Object.entries(q.fails)
  return (
    <section className="grid gap-10 md:grid-cols-2" data-quality>
      <div>
        <SectionLabel>质量</SectionLabel>
        <Line k="reworks" l="返工" sub={q.reworks_per_done != null ? `每件完成事项 ${q.reworks_per_done.toFixed(1)} 次` : undefined} v={q.reworks} />
        {fails.map(([k, n]) => <Line key={k} k={"fail-" + k} l={<span className="inline-flex items-center gap-2"><RED on={n > 0} />失败卡 · {FAIL_LABEL[k] ?? k}</span>} v={n} />)}
        <Line k="rollbacks" l={<span className="inline-flex items-center gap-2"><RED on={q.rollbacks > 0} />回滚</span>} sub={q.release_unknown ? `不知道现在版本 ${q.release_unknown}` : undefined} v={q.rollbacks} />
        <Line k="turn_errors" l={<span className="inline-flex items-center gap-2"><RED on={(q.turn_error_rate ?? 0) > 0} />回合出错率</span>} v={fmtPct(q.turn_error_rate, 1)} />
        {q.reopened > 0 && <Line k="reopened" l="重开" v={q.reopened} />}
      </div>
      <div>
        <SectionLabel>人的介入</SectionLabel>
        {dk.s !== "ok" ? <div className="py-6 type-callout text-muted-foreground">{dk.s === "loading" ? "…" : "读不到决定"}</div> : dk.v.rows.length === 0 ? <div className="py-4 type-callout text-muted-foreground">这段时间没有需要你决定的事。</div> : (
          <Table label="决定" min={320} head={<tr><th className={th}>种类</th><th className={cn(th, "pl-4 text-right")}>数量</th><th className={cn(th, "pl-4 text-right")}>响应中位</th><th className={cn(th, "pl-4 text-right")}>p85</th></tr>}>
            {dk.v.rows.map((r: CountRow) => (
              <tr key={r.key} data-decision-kind={r.key}><td className="py-2">{r.label}</td><td className={num}>{r.count}</td><td className={num}>{fmtMs(r.response?.p50)}</td><td className={num}>{fmtMs(r.response?.p85)}</td></tr>
            ))}
          </Table>
        )}
        <div className="mt-3 type-footnote text-muted-foreground" data-human>
          共 {h.decisions} 个决定 · 响应中位 {fmtMs(h.response.p50)} · p85 {fmtMs(h.response.p85)}{h.expired ? ` · 过期或作废 ${h.expired}` : ""} · 你补充的消息 {h.messages}{h.per_done != null ? ` · 每件完成事项介入 ${h.per_done.toFixed(1)} 次` : ""}
          {h.yolo && ` · YOLO 代办 ${h.yolo.decided}，拒绝代办 ${h.yolo.declined}，撤销 ${h.yolo.undone}`}
        </div>
      </div>
    </section>
  )
}

/** Agent */
export function AgentSection({ s, f, fkey, live }: { s: Summary; f: Filter; fkey: string; live: boolean }) {
  const [tk] = useStats(() => stats.counts(f, "tool"), [fkey], live)
  const [open, setOpen] = useState(false)
  const a = s.agents
  const rows = tk.s === "ok" ? tk.v.rows : []
  const kinds = rows.filter((r) => r.dim !== "tool"), names = rows.filter((r) => r.dim === "tool").slice(0, 10)
  const total = kinds.reduce((x, r) => x + r.count, 0)
  return (
    <section className="grid gap-10 md:grid-cols-2" data-agents>
      <div>
        <SectionLabel>Agent</SectionLabel>
        <Line k="turns" l="回合" sub={a.tokens_per_turn != null ? `每回合 ${fmtN(a.tokens_per_turn)} tokens` : undefined} v={fmtN(a.turns)} />
        <Line k="turn_ms" l="回合时长" sub={`p85 ${fmtMs(a.turn_ms.p85)}`} v={fmtMs(a.turn_ms.p50)} />
        <Line k="tools" l="工具调用" sub={a.tools_per_turn != null ? `每回合 ${a.tools_per_turn.toFixed(1)} 次` : undefined} v={fmtN(a.tools)} />
        <Line k="tool_errors" l={<span className="inline-flex items-center gap-2"><RED on={(a.tool_error_rate ?? 0) > 0} />工具失败率</span>} v={fmtPct(a.tool_error_rate, 1)} />
        <Line k="retries" l="网关 503 重试" sub={a.retry_success != null ? `重试后成功 ${fmtPct(a.retry_success)}` : undefined} v={a.retries} />
        <Line k="fallbacks" l="网关回退" sub={a.gateway_failed ? `失败 ${a.gateway_failed}` : undefined} v={a.gateway_fallbacks} />
        <Line k="interrupted" l="中断" sub={`Hub 重启 ${a.interrupted} · 你中断 ${a.cancelled}`} v={a.interrupted + a.cancelled} />
        <Line k="permissions" l="权限请求" v={a.permissions} />
      </div>
      <div>
        <SectionLabel>工具调用 <span className="font-normal">按种类</span></SectionLabel>
        {!kinds.length ? <div className="py-4 type-callout text-muted-foreground">这段时间没有工具调用。</div> : (
          <Table label="工具调用" min={300} head={<tr><th className={th}>种类</th><th className={cn(th, "pl-4 text-right")}>次数</th><th className={cn(th, "w-24 pl-4")}>占比</th></tr>}>
            {kinds.map((r) => <tr key={r.key} data-tool={r.key}><td className="py-2">{r.dim === "mcp" ? `Steve · ${r.label}` : r.label}</td><td className={num}>{r.count}</td><td className="py-2 pl-4"><Share r={total ? r.count / total : 0} /></td></tr>)}
          </Table>
        )}
        {names.length > 0 && (
          <div className="mt-3">
            <button type="button" className="inline-flex items-center gap-1 type-footnote text-muted-foreground hover:text-foreground" onClick={() => setOpen(!open)} aria-expanded={open} data-tool-names>
              {open ? <ChevronDownIcon className="size-3.5" /> : <ChevronRightIcon className="size-3.5" />}按工具名 Top {names.length}（与上表是同一批调用）
            </button>
            {open && <div className="mt-2">{names.map((r) => <div key={r.key} className="flex justify-between border-b py-1.5 type-callout last:border-0"><span className="font-mono text-[12.5px]">{r.label}</span><span className="tabular-nums">{r.count}</span></div>)}</div>}
          </div>
        )}
      </div>
    </section>
  )
}
