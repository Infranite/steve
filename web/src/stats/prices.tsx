/* 设置 · 统计（stats.md §4.1、§6.1）：价格（美元 / 百万 tokens）、切天的时区、重建统计。
 * 价格两个来源：价格表（自己的行或内置官方价，优先，可改、可恢复默认）和 Models.dev 自动同步的价格（表里没有的模型用它）。
 * 价格只用来估算，页面上的费用都标“估”。TS Hub 上这一节显示“还没有接入”。 */
import { useEffect, useState } from "react"
import { ChartNoAxesColumnIcon, PlusIcon, RefreshCwIcon, TrashIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { TextField } from "@/components/affect/text-field-clean"
import { SetSelect } from "@/settings-controls"
import { Spinner } from "@/components/ui/spinner"
import { stats, type Price, type Prices, type Quote } from "@/api/stats"
import { toast } from "@/store"
import { Group, Row, SectionLabel } from "@/ui"
import { t, useT } from "@/i18n"
import { num } from "@/i18n/format"
import { fmtN, time, useStats } from "@/stats/format"

/** the settings category: [id, title, icon, subtitle]. The title and subtitle are getters, so whoever reads them
 *  while rendering gets the current language (the tuple itself is built once, at import). */
export const STATS_CAT = Object.defineProperties(["stats", "", ChartNoAxesColumnIcon, ""] as [string, string, any, string], {
  1: { get: () => t("stats:page.title"), enumerable: true },
  3: { get: () => t("stats:prices.catSub"), enumerable: true },
})

const COLS = (): [keyof Price, string][] => [["input", t("stats:col.input")], ["output", t("stats:col.output")], ["cache_read", t("stats:col.cacheRead")], ["cache_write", t("stats:col.cacheWrite")]]
type Draft = { match: string; input: string; output: string; cache_read: string; cache_write: string; note: string }
const toDraft = (p: Price): Draft => ({ match: p.match, input: String(p.input), output: String(p.output), cache_read: String(p.cache_read), cache_write: String(p.cache_write), note: p.note ?? "" })
const ZONES = ["Asia/Shanghai", "Asia/Tokyo", "Asia/Singapore", "Europe/London", "Europe/Berlin", "America/New_York", "America/Los_Angeles", "UTC"]

const money = (n: number) => String(Number(n.toFixed(4)))

/** where a model's price comes from, in words */
export function sourceText(q: Quote | null | undefined): string {
  if (!q) return t("stats:word.unpriced")
  const text = q.source === "models_dev" ? t("stats:prices.srcMd", { match: q.match }) : t("stats:prices.srcTable", { match: q.match })
  return q.via ? t("stats:prices.via", { text, via: q.via }) : text
}

/** Models.dev 自动同步：上次同步、失败原因（继续用上次的价格）、立即同步 */
function CatalogSync({ v, reload }: { v: Prices; reload: () => void }) {
  const t = useT()
  const [busy, setBusy] = useState(false)
  const c = v.catalog
  if (!c) return null
  const sync = async () => {
    setBusy(true)
    try {
      const r = await stats.syncPrices()
      toast(r.unchanged ? t("stats:prices.unchanged") : r.changed ? t("stats:prices.syncedChanged", { count: r.models ?? 0, n: fmtN(r.models ?? 0), changed: fmtN(r.changed) }) : t("stats:prices.synced", { count: r.models ?? 0, n: fmtN(r.models ?? 0) }))
    } catch (e) { toast(e instanceof Error ? e.message : String(e)) } finally { setBusy(false); reload() }
  }
  const status = !c.enabled ? t("stats:prices.disabled")
    : c.synced ? t("stats:prices.lastSync", { time: time(c.synced), count: c.models, n: num(c.models) })
    : c.error ? t("stats:prices.neverOk") : t("stats:prices.notYet")
  return (
    <section data-catalog>
      <SectionLabel right={<a href="https://models.dev" target="_blank" rel="noreferrer" className="type-footnote text-blue hover:underline">models.dev</a>}>{t("stats:prices.catalog")}</SectionLabel>
      <p className="mb-2 type-callout text-muted-foreground">{t("stats:prices.catalogIntro")}</p>
      <Group>
        <Row title={t("stats:prices.syncStatus")} sub={<span data-catalog-status>{status}</span>}>
          {c.enabled && <Button size="sm" variant="outline" disabled={busy} onClick={() => void sync()} data-catalog-sync>{busy ? <Spinner /> : <RefreshCwIcon />}{t("stats:prices.syncNow")}</Button>}
        </Row>
      </Group>
      {c.enabled && c.error && (
        <div className="anim-in mt-2 rounded-lg bg-attention/10 px-3 py-2 type-callout text-attention" role="alert" data-catalog-error>
          {c.tried
            ? t(c.synced ? "stats:prices.syncErrorKeepAt" : "stats:prices.syncErrorTableAt", { time: time(c.tried), error: c.error })
            : t(c.synced ? "stats:prices.syncErrorKeep" : "stats:prices.syncErrorTable", { error: c.error })}
        </div>
      )}
    </section>
  )
}

/** the field a 400 names (`参数不对：prices[2].input`) → [row, column] */
const badField = (msg: string): [number, string] | null => { const m = msg.match(/prices\[(\d+)\]\.(\w+)/); return m ? [Number(m[1]), m[2]] : null }

export function StatsSettingsPane() {
  const t = useT()
  const [d, reload] = useStats(() => stats.prices(), [], false)
  const [rows, setRows] = useState<Draft[] | null>(null)
  const [busy, setBusy] = useState(false)
  const [bad, setBad] = useState<[number, string] | null>(null)
  const [err, setErr] = useState("")
  useEffect(() => { if (d.s === "ok") setRows(d.v.prices.map(toDraft)) }, [d])
  if (d.s === "loading" || (d.s === "ok" && !rows)) return <div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div>
  if (d.s === "absent") return <div className="rounded-xl bg-card px-4 py-3.5 type-callout text-muted-foreground shadow-[var(--lift)]" data-stats-unwired>{t("stats:prices.unwired")}</div>
  if (d.s === "error") return <div className="type-callout text-muted-foreground" role="alert">{d.msg}</div>
  const v = d.v, list = rows!, cols = COLS()
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
    void save(list.map((r) => ({ match: r.match.trim(), input: r.input.trim() === "" ? NaN : Number(r.input), output: r.output.trim() === "" ? NaN : Number(r.output), cache_read: r.cache_read.trim() === "" ? NaN : Number(r.cache_read), cache_write: r.cache_write.trim() === "" ? NaN : Number(r.cache_write), note: r.note })).map((p) => Object.fromEntries(Object.entries(p).map(([k, x]) => [k, typeof x === "number" && !isFinite(x) ? null : x])) as Price), t("stats:prices.saved"))
  }
  const add = (match = "", q?: Quote | null) => setRows([...list, q
    ? { match, input: money(q.input), output: money(q.output), cache_read: money(q.cache_read), cache_write: money(q.cache_write), note: t("stats:prices.fromNote", { source: sourceText(q) }) }
    : { match, input: "", output: "", cache_read: "", cache_write: "", note: "" }])
  const inTable = (m: string) => list.some((r) => r.match.trim() === m)
  return (
    <div className="flex flex-col gap-10" data-stats-settings>
      {v.models.length > 0 && (
        <section data-price-models>
          <SectionLabel right={<span className="type-footnote">{t("stats:prices.perMillion")}</span>}>{t("stats:prices.models")}</SectionLabel>
          <p className="mb-3 type-callout text-muted-foreground">{t("stats:prices.modelsIntro")}</p>
          <div className="-mx-4 overflow-x-auto px-4 sm:mx-0 sm:px-0">
            <table className="w-full min-w-[620px] type-callout" aria-label={t("stats:prices.modelsAria")}>
              <thead className="border-b text-left type-footnote text-muted-foreground"><tr><th className="py-2 font-medium">{t("stats:col.model")}</th><th className="py-2 pl-2 font-medium">{t("stats:col.source")}</th>{cols.map(([, l]) => <th key={l} className="whitespace-nowrap py-2 pl-3 text-right font-medium">{l}</th>)}<th className="whitespace-nowrap py-2 pl-3 text-right font-medium">tokens</th><th className="w-16" /></tr></thead>
              <tbody>{v.models.map((m) => {
                const q = m.price
                return (
                  <tr key={m.model} className="border-b last:border-0" data-price-model={m.model} data-price-source={q?.source ?? "none"}>
                    <td className="max-w-56 truncate py-2.5 type-code" title={m.model}>{m.model || t("stats:word.unknownModel")}</td>
                    <td className="min-w-40 max-w-64 py-2 pl-2" title={q?.note || undefined}>
                      {q ? <span className={q.source === "models_dev" ? "text-foreground" : "text-muted-foreground"}>{q.source === "models_dev" ? "Models.dev" : t("stats:prices.table")}</span> : <span className="text-attention">{t("stats:word.unpriced")}</span>}
                      {q?.via && <span className="ml-1 type-footnote text-muted-foreground">{t("stats:prices.estBy", { match: q.match })}</span>}
                      {q?.note && <div className="type-footnote text-muted-foreground">{q.note}</div>}
                    </td>
                    {cols.map(([k]) => <td key={k} className="py-2 pl-3 text-right tabular-nums">{q ? money(q[k] as number) : "—"}</td>)}
                    <td className="py-2 pl-3 text-right tabular-nums text-muted-foreground">{fmtN(m.tokens)}</td>
                    <td className="py-2 pl-1 text-right">{m.model && q?.source !== "table" && <Button size="xs" variant="ghost" className="text-blue" disabled={inTable(m.model)} onClick={() => add(m.model, q)} data-price-override={m.model}>{q ? t("stats:prices.override") : t("stats:prices.addPrice")}</Button>}</td>
                  </tr>
                )
              })}</tbody>
            </table>
          </div>
        </section>
      )}
      <CatalogSync v={v} reload={reload} />
      <section>
        <SectionLabel right={v.custom ? <span className="type-footnote">{t("stats:prices.custom")}</span> : <span className="type-footnote">{t("stats:prices.default")}</span>}>{t("stats:prices.table")} <span className="font-normal">{t("stats:prices.tableSub")}</span></SectionLabel>
        <p className="mb-3 type-callout text-muted-foreground">{t("stats:prices.tableIntro")}</p>
        <div className="-mx-4 overflow-x-auto px-4 sm:mx-0 sm:px-0">
          <table className="w-full min-w-[620px] type-callout" aria-label={t("stats:prices.table")}>
            <thead className="border-b text-left type-footnote text-muted-foreground"><tr><th className="py-2 font-medium">{t("stats:col.model")}</th>{cols.map(([, l]) => <th key={l} className="w-20 py-2 pl-2 font-medium">{l}</th>)}<th className="py-2 pl-2 font-medium">{t("stats:col.note")}</th><th className="w-8" /></tr></thead>
            <tbody>{list.map((r, i) => (
              <tr key={i} className="border-b last:border-0" data-price-row={i}>
                <td className="py-1.5"><TextField size="sm" value={r.match} onChange={(e) => set(i, "match", e.target.value)} aria-label={t("stats:prices.rowModel", { n: i + 1 })} error={bad?.[0] === i && bad[1] === "match"} inputClassName="type-code" placeholder={t("stats:prices.matchPlaceholder")} /></td>
                {cols.map(([k, l]) => <td key={k} className="py-1.5 pl-2"><TextField size="sm" inputMode="decimal" value={r[k as keyof Draft]} onChange={(e) => set(i, k as keyof Draft, e.target.value)} aria-label={t("stats:prices.rowPrice", { n: i + 1, col: l })} error={bad?.[0] === i && bad[1] === k} inputClassName="text-right tabular-nums" /></td>)}
                <td className="py-1.5 pl-2"><TextField size="sm" value={r.note} onChange={(e) => set(i, "note", e.target.value)} aria-label={t("stats:prices.rowNote", { n: i + 1 })} /></td>
                <td className="py-1.5 pl-1"><Button size="icon-xs" variant="ghost" className="text-muted-foreground" aria-label={t("stats:prices.rowDelete", { n: i + 1 })} onClick={() => setRows(list.filter((_, n) => n !== i))}><TrashIcon /></Button></td>
              </tr>
            ))}</tbody>
          </table>
        </div>
        {err && <div className="mt-2 type-footnote text-destructive" role="alert" data-price-error>{err}</div>}
        <div className="mt-3 flex flex-wrap items-center gap-2">
          <Button size="sm" variant="ghost" onClick={() => add()} data-price-add><PlusIcon />{t("stats:prices.addRow")}</Button>
          <span className="flex-1" />
          {v.custom && <Button size="sm" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void save(null, t("stats:prices.reset"))} data-price-reset>{t("stats:prices.resetButton")}</Button>}
          <Button size="sm" disabled={busy || !dirty} onClick={submit} data-price-save>{busy && <Spinner />}{t("common:action.save")}</Button>
        </div>
      </section>
      {v.unpriced.length > 0 && (
        <section data-unpriced>
          <SectionLabel>{t("stats:prices.unpriced")}</SectionLabel>
          <p className="mb-2 type-callout text-muted-foreground">{t("stats:prices.unpricedIntro")}</p>
          <Group>{v.unpriced.map((u) => (
            <Row key={u.model} title={<span className="type-code">{u.model || t("stats:word.unknownModel")}</span>} sub={t("stats:prices.unpricedTokens", { n: fmtN(u.tokens) })}>
              {u.model && <Button size="xs" variant="ghost" className="text-blue" onClick={() => add(u.model)} disabled={inTable(u.model)}>{t("stats:prices.addPrice")}</Button>}
            </Row>
          ))}</Group>
        </section>
      )}
      <section>
        <SectionLabel>{t("stats:prices.tzSection")}</SectionLabel>
        <Group>
          <Row title={t("stats:prices.tz")} sub={v.hub_tz ? t("stats:prices.tzSubHub", { tz: v.hub_tz }) : t("stats:prices.tzSub")}>
            <SetSelect value={v.tz ?? ""} testId="stats-tz" label={t("stats:prices.tz")} className="w-56" onChange={(z) => { void stats.setTz(z || null).then(() => { toast(t("stats:prices.tzChanged")); reload() }, (e) => toast(e instanceof Error ? e.message : String(e))) }}
              options={[{ v: "", l: v.hub_tz ? t("stats:prices.followHubTz", { tz: v.hub_tz }) : t("stats:prices.followHub") }, ...[...new Set([...(v.tz ? [v.tz] : []), ...ZONES])].map((z) => ({ v: z, l: z }))]} />
          </Row>
          <Row title={t("stats:prices.rebuild")} sub={t("stats:prices.rebuildSub")}>
            <Button size="sm" variant="outline" onClick={() => void stats.rebuild().then(() => toast(t("stats:prices.rebuilding")), (e) => toast(e instanceof Error ? e.message : String(e)))} data-stats-rebuild>{t("stats:prices.rebuildButton")}</Button>
          </Row>
        </Group>
      </section>
    </div>
  )
}
