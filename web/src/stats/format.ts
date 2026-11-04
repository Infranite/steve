/* 统计页的数字格式（stats.md §6.2）：大数缩写、美元两位（不到 1 分写“<$0.01”）、时长“3 小时 12 分”。 */
import { useCallback, useEffect, useRef, useState } from "react"
import { NotWired, Unauthenticated } from "@/api"
import { useHubEvents } from "@/store"
import type { Filter } from "@/api/stats"

export const fmtN = (n: number | null | undefined) => {
  if (n == null || !isFinite(n)) return "—"
  const a = Math.abs(n)
  if (a >= 1e9) return (n / 1e9).toFixed(a >= 1e10 ? 0 : 1).replace(/\.0$/, "") + "B"
  if (a >= 1e6) return (n / 1e6).toFixed(a >= 1e7 ? 0 : 1).replace(/\.0$/, "") + "M"
  if (a >= 1e3) return (n / 1e3).toFixed(a >= 1e4 ? 0 : 1).replace(/\.0$/, "") + "K"
  return String(Math.round(n * 10) / 10)
}
export const fmtUsd = (n: number | null | undefined) => (n == null || !isFinite(n) ? "—" : n === 0 ? "$0" : n < 0.01 ? "<$0.01" : "$" + (n >= 1000 ? Math.round(n).toLocaleString("en-US") : n.toFixed(2)))
export const fmtPct = (r: number | null | undefined, digits = 0) => (r == null || !isFinite(r) ? "—" : (r * 100).toFixed(digits) + "%")
export const fmtMs = (ms: number | null | undefined) => {
  if (ms == null || !isFinite(ms)) return "—"
  const m = Math.round(ms / 60000)
  if (ms < 60000) return Math.max(0, Math.round(ms / 1000)) + " 秒"
  if (m < 60) return m + " 分钟"
  const h = Math.floor(m / 60)
  if (h < 48) return h + " 小时" + (m % 60 ? ` ${m % 60} 分` : "")
  const d = Math.floor(h / 24)
  return d + " 天" + (h % 24 ? ` ${h % 24} 小时` : "")
}
/** a change against the previous range, as grey text: “↑ 18%”, “↓ 3%”, “持平”, or `null` when there is nothing to compare */
export const delta = (now: number | null | undefined, before: number | null | undefined) => {
  if (now == null || before == null || !isFinite(now) || !isFinite(before)) return null
  if (before === 0) return now === 0 ? "持平" : null
  const r = (now - before) / Math.abs(before)
  if (Math.abs(r) < 0.005) return "持平"
  return (r > 0 ? "↑ " : "↓ ") + Math.abs(r * 100).toFixed(Math.abs(r) < 0.1 ? 1 : 0) + "%"
}
export const time = (ts: number) => { const d = new Date(ts); return `${d.getMonth() + 1}月${d.getDate()}日 ${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}` }
export const hhmm = (ts: number) => { const d = new Date(ts); return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}` }
export const shortDay = (d: string) => { const [, m, x] = d.split("-"); return `${Number(m)}/${Number(x)}` }
/* 小时（ST-43）：单日范围（今天，或选的某一天）的趋势按统计时区的整点小时，bucket 是 `YYYY-MM-DDTHH:00` */
/** the range is one day, so its charts go by hour */
export const oneDay = (f: Filter) => f.range === "today" || (f.range === "custom" && !!f.from && f.from === f.to)
export const isHour = (b: string) => b.length > 10 && b[10] === "T"
/** “14:00–15:00” (the last one “23:00–24:00”) */
export const hourSpan = (b: string) => { const h = Number(b.slice(11, 13)); return `${String(h).padStart(2, "0")}:00–${String(h + 1).padStart(2, "0")}:00` }
/** a bucket, said in full: “11月5日 14:00–15:00”, or the day “11月5日” */
export const bucketName = (b: string) => isHour(b) ? `${fmtRange(b.slice(0, 10), b.slice(0, 10))} ${hourSpan(b)}` : fmtRange(b, b)
/** the bucket under the axis: “14:00”, or the day “11/5” */
export const bucketTick = (b: string) => (isHour(b) ? b.slice(11, 16) : shortDay(b))
/** the zone an hourly chart is cut in, for its subtitle: “北京时间” for Asia/Shanghai, else the IANA name */
export const zoneName = (tz?: string | null) => (!tz || tz === "local" ? "Hub 所在时区" : tz === "Asia/Shanghai" ? "北京时间" : tz)

export type Load<T> = { s: "loading" } | { s: "absent" } | { s: "error"; msg: string } | { s: "ok"; v: T; at: number }
/** loads `f` now, whenever `deps` change and on every `stats` stream event (`live` = only while it can matter) */
export function useStats<T>(f: () => Promise<T>, deps: unknown[], live = true): [Load<T>, () => void] {
  const [v, setV] = useState<Load<T>>({ s: "loading" })
  const seq = useRef(0)
  const ref = useRef(f); ref.current = f
  const load = useCallback(() => {
    const n = ++seq.current
    ref.current().then((x) => n === seq.current && setV({ s: "ok", v: x, at: Date.now() }), (e) => {
      if (n !== seq.current) return
      if (e instanceof Unauthenticated) { location.reload(); return }
      setV(e instanceof NotWired ? { s: "absent" } : { s: "error", msg: e instanceof Error ? e.message : String(e) })
    })
  }, [])
  useEffect(() => { load() }, deps) // eslint-disable-line react-hooks/exhaustive-deps
  useHubEvents(["stats"], () => { if (live) load() }, 200)
  return [v, load]
}
/** the range a chart covers, said the same way on every chart: “11月1日 – 11月7日”, one day as “11月5日” */
export const fmtRange = (from: string, to: string) => {
  const d = (x: string) => { const [, m, day] = x.split("-"); return `${Number(m)}月${Number(day)}日` }
  return from === to ? d(from) : `${d(from)} – ${d(to)}`
}
/** a duration axis in one unit chosen by the largest value, so the ticks read “2 小时”, not “1 小时 57 分” */
export const durUnit = (maxMs: number): [string, number] => (maxMs >= 2 * 86400000 ? ["天", 86400000] : maxMs >= 2 * 3600000 ? ["小时", 3600000] : ["分钟", 60000])
