/* 设置 · 统计（stats.md §4.1、§6.1）：价格（美元 / 百万 tokens）、切天的时区、重建统计。
 * 价格两个来源：价格表（自己的行或内置官方价，优先，可改、可恢复默认）和 Models.dev 自动同步的价格（表里没有的模型用它）。
 * 价格只用来估算，页面上的费用都标“估”。TS Hub 上这一节显示“还没有接入”。 */
import { useEffect, useState } from "react"
import { ChartNoAxesColumnIcon, PlusIcon, RefreshCwIcon, TrashIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { stats, type Price, type Prices, type Quote } from "@/api/stats"
import { toast } from "@/store"
import { Group, Row, SectionLabel } from "@/ui"
import { Select } from "@/view-shared"
import { fmtN, time, useStats } from "@/stats/format"

export const STATS_CAT: [string, string, any, string] = ["stats", "统计", ChartNoAxesColumnIcon, "价格、时区、重建"]

const COLS: [keyof Price, string][] = [["input", "输入"], ["output", "输出"], ["cache_read", "缓存读"], ["cache_write", "缓存写"]]
type Draft = { match: string; input: string; output: string; cache_read: string; cache_write: string; note: string }
const toDraft = (p: Price): Draft => ({ match: p.match, input: String(p.input), output: String(p.output), cache_read: String(p.cache_read), cache_write: String(p.cache_write), note: p.note ?? "" })
const ZONES = ["Asia/Shanghai", "Asia/Tokyo", "Asia/Singapore", "Europe/London", "Europe/Berlin", "America/New_York", "America/Los_Angeles", "UTC"]

const money = (n: number) => String(Number(n.toFixed(4)))

/** where a model's price comes from, in words */
export function sourceText(q: Quote | null | undefined): string {
  if (!q) return "未定价"
  const via = q.via ? `（按 ${q.via} 后面的模型价估）` : ""
  return (q.source === "models_dev" ? `Models.dev · ${q.match}` : `价格表 · ${q.match}`) + via
}

/** Models.dev 自动同步：上次同步、失败原因（继续用上次的价格）、立即同步 */
function CatalogSync({ v, reload }: { v: Prices; reload: () => void }) {
  const [busy, setBusy] = useState(false)
  const c = v.catalog
  if (!c) return null
  const sync = async () => {
    setBusy(true)
    try {
      const r = await stats.syncPrices()
      toast(r.unchanged ? "Models.dev 的价格没有变化" : `已同步 ${fmtN(r.models ?? 0)} 个模型的价格${r.changed ? `，${fmtN(r.changed)} 条有新增或变动` : ""}`)
    } catch (e) { toast(e instanceof Error ? e.message : String(e)) } finally { setBusy(false); reload() }
  }
  const status = !c.enabled ? "已关闭（STEVE_PRICE_CATALOG_URL=off），只用价格表"
    : c.synced ? `上次同步 ${time(c.synced)}，收录 ${c.models.toLocaleString("en-US")} 个模型；每天自动更新`
    : c.error ? "还没有同步成功过" : "还没有同步，Hub 启动后会自动同步"
  return (
    <section data-catalog>
      <SectionLabel right={<a href="https://models.dev" target="_blank" rel="noreferrer" className="type-footnote text-blue hover:underline">models.dev</a>}>自动价格 · Models.dev</SectionLabel>
      <p className="mb-2 type-callout text-muted-foreground">价格表里没有的模型，按供应商和模型编号匹配 Models.dev 的公开价格（开源数据，MIT 许可）。价格有变动时从同步后的第二天起生效，已经统计的日子不变。</p>
      <Group>
        <Row title="同步状态" sub={<span data-catalog-status>{status}</span>}>
          {c.enabled && <Button size="sm" variant="outline" disabled={busy} onClick={() => void sync()} data-catalog-sync>{busy ? <Spinner /> : <RefreshCwIcon />}立即同步</Button>}
        </Row>
      </Group>
      {c.enabled && c.error && (
        <div className="mt-2 rounded-lg border border-attention/40 bg-attention/5 px-3 py-2 type-footnote" role="alert" data-catalog-error>
          {c.tried ? `${time(c.tried)} ` : ""}同步失败：{c.error}。{c.synced ? "继续使用上次同步的价格。" : "暂时只用价格表的价格。"}
        </div>
      )}
    </section>
  )
}

/** the field a 400 names (`参数不对：prices[2].input`) → [row, column] */
const badField = (msg: string): [number, string] | null => { const m = msg.match(/prices\[(\d+)\]\.(\w+)/); return m ? [Number(m[1]), m[2]] : null }

export function StatsSettingsPane() {
  const [d, reload] = useStats(() => stats.prices(), [], false)
  const [rows, setRows] = useState<Draft[] | null>(null)
  const [busy, setBusy] = useState(false)
  const [bad, setBad] = useState<[number, string] | null>(null)
  const [err, setErr] = useState("")
  useEffect(() => { if (d.s === "ok") setRows(d.v.prices.map(toDraft)) }, [d])
  if (d.s === "loading" || (d.s === "ok" && !rows)) return <div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div>
  if (d.s === "absent") return <div className="rounded-xl border px-4 py-3.5 type-callout text-muted-foreground" data-stats-unwired>这个 Hub 还没有统计模块，换成新版 Hub 后可以在这里改价格表。</div>
  if (d.s === "error") return <div className="type-callout text-muted-foreground" role="alert">{d.msg}</div>
  const v = d.v, list = rows!
  const set = (i: number, k: keyof Draft, val: string) => { setBad(null); setErr(""); setRows(list.map((r, n) => (n === i ? { ...r, [k]: val } : r))) }
  const dirty = JSON.stringify(list) !== JSON.stringify(v.prices.map(toDraft))
  const save = async (prices: Price[] | null, ok: string) => {
    setBusy(true); setErr(""); setBad(null)
    try { await stats.setPrices(prices); toast(ok); reload() } catch (e) {
      const m = e instanceof Error ? e.message : String(e); setErr(m); setBad(badField(m))
    } finally { setBusy(false) }
  }
  const submit = () => {
    // numbers as typed; the Hub names a bad one
    void save(list.map((r) => ({ match: r.match.trim(), input: r.input.trim() === "" ? NaN : Number(r.input), output: r.output.trim() === "" ? NaN : Number(r.output), cache_read: r.cache_read.trim() === "" ? NaN : Number(r.cache_read), cache_write: r.cache_write.trim() === "" ? NaN : Number(r.cache_write), note: r.note })).map((p) => Object.fromEntries(Object.entries(p).map(([k, x]) => [k, typeof x === "number" && !isFinite(x) ? null : x])) as Price), "价格表已保存，统计页的估算随之更新")
  }
  const add = (match = "", q?: Quote | null) => setRows([...list, q
    ? { match, input: money(q.input), output: money(q.output), cache_read: money(q.cache_read), cache_write: money(q.cache_write), note: `从 ${sourceText(q)} 改` }
    : { match, input: "", output: "", cache_read: "", cache_write: "", note: "" }])
  const inTable = (m: string) => list.some((r) => r.match.trim() === m)
  return (
    <div className="flex flex-col gap-10" data-stats-settings>
      {v.models.length > 0 && (
        <section data-price-models>
          <SectionLabel right={<span className="type-footnote">美元 / 百万 tokens</span>}>用到的模型和价格来源</SectionLabel>
          <p className="mb-3 type-callout text-muted-foreground">费用 = tokens × 单价，是估算，不是账单，实际以服务商账单为准。先用价格表，表里没有的用 Models.dev；都没有的是“未定价”，它们的 tokens 不计入估算，不当作免费。</p>
          <div className="-mx-4 overflow-x-auto px-4 sm:mx-0 sm:px-0">
            <table className="w-full min-w-[620px] type-callout" aria-label="用到的模型">
              <thead className="border-b text-left type-footnote text-muted-foreground"><tr><th className="py-2 font-medium">模型</th><th className="py-2 pl-2 font-medium">来源</th>{COLS.map(([, l]) => <th key={l} className="py-2 pl-2 text-right font-medium">{l}</th>)}<th className="py-2 pl-2 text-right font-medium">tokens</th><th className="w-16" /></tr></thead>
              <tbody>{v.models.map((m) => {
                const q = m.price
                return (
                  <tr key={m.model} className="border-b last:border-0" data-price-model={m.model} data-price-source={q?.source ?? "none"}>
                    <td className="max-w-56 truncate py-2 font-mono text-[12.5px]" title={m.model}>{m.model || "未知模型"}</td>
                    <td className="py-2 pl-2" title={q?.note || undefined}>
                      {q ? <span className={q.source === "models_dev" ? "text-foreground" : "text-muted-foreground"}>{q.source === "models_dev" ? "Models.dev" : "价格表"}</span> : <span className="text-attention">未定价</span>}
                      {q?.via && <span className="ml-1 type-footnote text-muted-foreground">按 {q.match} 估</span>}
                      {q?.note && <div className="type-footnote text-muted-foreground">{q.note}</div>}
                    </td>
                    {COLS.map(([k]) => <td key={k} className="py-2 pl-2 text-right tabular-nums">{q ? money(q[k] as number) : "—"}</td>)}
                    <td className="py-2 pl-2 text-right tabular-nums text-muted-foreground">{fmtN(m.tokens)}</td>
                    <td className="py-2 pl-1 text-right">{m.model && q?.source !== "table" && <Button size="xs" variant="ghost" className="text-blue" disabled={inTable(m.model)} onClick={() => add(m.model, q)} data-price-override={m.model}>{q ? "改价" : "加价格"}</Button>}</td>
                  </tr>
                )
              })}</tbody>
            </table>
          </div>
        </section>
      )}
      <CatalogSync v={v} reload={reload} />
      <section>
        <SectionLabel right={v.custom ? <span className="type-footnote">已改过</span> : <span className="type-footnote">默认（官方标价）</span>}>价格表 <span className="font-normal">手动，优先 · 美元 / 百万 tokens</span></SectionLabel>
        <p className="mb-3 type-callout text-muted-foreground">这张表的价格优先于 Models.dev，代理、自建服务或有折扣的模型在这里单独写。模型按编号精确匹配，或用末尾 * 匹配前缀；按顺序取第一条。kiro/… 没有自己的一行时按去掉 kiro/ 的模型算（Kiro 实际计费未知）。改这张表会重算全部历史估算。</p>
        <div className="-mx-4 overflow-x-auto px-4 sm:mx-0 sm:px-0">
          <table className="w-full min-w-[620px] type-callout" aria-label="价格表">
            <thead className="border-b text-left type-footnote text-muted-foreground"><tr><th className="py-2 font-medium">模型</th>{COLS.map(([, l]) => <th key={l} className="w-20 py-2 pl-2 font-medium">{l}</th>)}<th className="py-2 pl-2 font-medium">备注</th><th className="w-8" /></tr></thead>
            <tbody>{list.map((r, i) => (
              <tr key={i} className="border-b last:border-0" data-price-row={i}>
                <td className="py-1.5"><Input value={r.match} onChange={(e) => set(i, "match", e.target.value)} aria-label={`第 ${i + 1} 行模型`} aria-invalid={bad?.[0] === i && bad[1] === "match"} className="h-8 font-mono text-[12.5px]" placeholder="claude-opus-5-5 或 gpt-*" /></td>
                {COLS.map(([k, l]) => <td key={k} className="py-1.5 pl-2"><Input inputMode="decimal" value={r[k as keyof Draft]} onChange={(e) => set(i, k as keyof Draft, e.target.value)} aria-label={`第 ${i + 1} 行${l}价格`} aria-invalid={bad?.[0] === i && bad[1] === k} className="h-8 text-right tabular-nums" /></td>)}
                <td className="py-1.5 pl-2"><Input value={r.note} onChange={(e) => set(i, "note", e.target.value)} aria-label={`第 ${i + 1} 行备注`} className="h-8" /></td>
                <td className="py-1.5 pl-1"><Button size="icon-xs" variant="ghost" className="text-muted-foreground" aria-label={`删除第 ${i + 1} 行`} onClick={() => setRows(list.filter((_, n) => n !== i))}><TrashIcon /></Button></td>
              </tr>
            ))}</tbody>
          </table>
        </div>
        {err && <div className="mt-2 type-footnote text-destructive" role="alert" data-price-error>{err}</div>}
        <div className="mt-3 flex flex-wrap items-center gap-2">
          <Button size="sm" variant="ghost" onClick={() => add()} data-price-add><PlusIcon />加一行</Button>
          <span className="flex-1" />
          {v.custom && <Button size="sm" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void save(null, "已恢复默认价格")} data-price-reset>恢复默认</Button>}
          <Button size="sm" disabled={busy || !dirty} onClick={submit} data-price-save>{busy && <Spinner />}保存</Button>
        </div>
      </section>
      {v.unpriced.length > 0 && (
        <section data-unpriced>
          <SectionLabel>未定价的模型</SectionLabel>
          <p className="mb-2 type-callout text-muted-foreground">价格表和 Models.dev 都没有这些模型的价格，它们的 tokens 不计入估算费用，费用是不完整的。</p>
          <Group>{v.unpriced.map((u) => (
            <Row key={u.model} title={<span className="font-mono text-[13px]">{u.model || "未知模型"}</span>} sub={`${fmtN(u.tokens)} tokens 未计入`}>
              {u.model && <Button size="xs" variant="ghost" className="text-blue" onClick={() => add(u.model)} disabled={inTable(u.model)}>加价格</Button>}
            </Row>
          ))}</Group>
        </section>
      )}
      <section>
        <SectionLabel>按哪个时区切天</SectionLabel>
        <Group>
          <Row title="时区" sub={`默认是 Hub 所在机器的时区${v.hub_tz ? `（${v.hub_tz}）` : ""}。改了以后按新时区重建统计。`}>
            <Select value={v.tz ?? ""} align="end" testId="stats-tz" onChange={(z) => { void stats.setTz(z || null).then(() => { toast("已改时区，正在按新时区重建统计"); reload() }, (e) => toast(e instanceof Error ? e.message : String(e))) }}
              options={[{ v: "", l: `跟随 Hub${v.hub_tz ? `（${v.hub_tz}）` : ""}` }, ...[...new Set([...(v.tz ? [v.tz] : []), ...ZONES])].map((z) => ({ v: z, l: z }))]} />
          </Row>
          <Row title="重建统计" sub="按明细重新汇总每天的数字，并重算每件事项。只有工作空间的所有者可以。">
            <Button size="sm" variant="outline" onClick={() => void stats.rebuild().then(() => toast("正在重建，完成后统计页自动刷新"), (e) => toast(e instanceof Error ? e.message : String(e)))} data-stats-rebuild>重建</Button>
          </Row>
        </Group>
      </section>
    </div>
  )
}
