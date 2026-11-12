/* 统计页的数字格式（stats.md §6.2）：大数缩写、美元两位（不到 1 分写“<$0.01”）、时长“3 小时 12 分”。 */
import { useCallback, useEffect, useRef, useState } from "react"
import { NotWired, Unauthenticated } from "@/api"
import { useHubEvents } from "@/store"
import type { Filter } from "@/api/stats"
import { t } from "@/i18n"
import { dayRange, durUnit, longDur, monthDayTime } from "@/i18n/format"

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
  return longDur(ms)
}
/** a change against the previous range, as grey text: “↑ 18%”, “↓ 3%”, “持平”, or `null` when there is nothing to compare */
export const delta = (now: number | null | undefined, before: number | null | undefined) => {
  if (now == null || before == null || !isFinite(now) || !isFinite(before)) return null
  if (before === 0) return now === 0 ? t("stats:delta.flat") : null
  const r = (now - before) / Math.abs(before)
  if (Math.abs(r) < 0.005) return t("stats:delta.flat")
  const pct = Math.abs(r * 100).toFixed(Math.abs(r) < 0.1 ? 1 : 0)
  return r > 0 ? t("stats:delta.up", { pct }) : t("stats:delta.down", { pct })
}
export const time = (ts: number) => monthDayTime(ts)
export const hhmm = (ts: number) => { const d = new Date(ts); return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}` }
export const shortDay = (d: string) => { const [, m, x] = d.split("-"); return `${Number(m)}/${Number(x)}` }
/* 小时（ST-43）：单日范围（今天，或选的某一天）的趋势按统计时区的整点小时，bucket 是 `YYYY-MM-DDTHH:00` */
/** the range is one day, so its charts go by hour */
export const oneDay = (f: Filter) => f.range === "today" || (f.range === "custom" && !!f.from && f.from === f.to)
export const isHour = (b: string) => b.length > 10 && b[10] === "T"
/** “14:00–15:00” (the last one “23:00–24:00”) */
export const hourSpan = (b: string) => { const h = Number(b.slice(11, 13)); return `${String(h).padStart(2, "0")}:00–${String(h + 1).padStart(2, "0")}:00` }
/** a bucket, said in full: “11月5日 14:00–15:00”, or the day “11月5日” */
export const bucketName = (b: string) => isHour(b) ? t("common:time.dateAt", { date: fmtRange(b.slice(0, 10), b.slice(0, 10)), time: hourSpan(b) }) : fmtRange(b, b)
/** the bucket under the axis: “14:00”, or the day “11/5” */
export const bucketTick = (b: string) => (isHour(b) ? b.slice(11, 16) : shortDay(b))
/** the zone an hourly chart is cut in, for its subtitle: “北京时间” for Asia/Shanghai, else the IANA name */
export const zoneName = (tz?: string | null) => (!tz || tz === "local" ? t("stats:zone.hub") : tz === "Asia/Shanghai" ? t("stats:zone.beijing") : tz)

/* 时间轴的刻度（事件浏览器、事项报告的时间轴）：整点、整分或本地零点，不是跨度的几分之几 */
const DAY = 86400000
/** tick steps of the time axis: round minutes and hours, then whole days */
const STEPS = [60000, 300000, 900000, 1800000, 3600000, 3 * 3600000, 6 * 3600000, 12 * 3600000, DAY, 2 * DAY, 7 * DAY, 14 * DAY]
/** round local times in [lo, hi) for an axis `w` pixels wide: whole hours or local midnights, not fractions of the span
 *  (the span ends at the next midnight, which belongs to the day after, so it gets no tick) */
export function timeTicks(lo: number, hi: number, w: number): number[] {
  const want = Math.max(4, Math.floor(w / 90))
  const step = STEPS.find((s) => (hi - lo) / s <= want) ?? STEPS[STEPS.length - 1]
  const out: number[] = []
  if (step < DAY) {
    const d = new Date(lo); d.setHours(0, 0, 0, 0)
    for (let ts = d.getTime(); ts < hi && out.length < 200; ts += step) if (ts >= lo) out.push(ts)
  } else {
    const d = new Date(lo); d.setHours(0, 0, 0, 0)
    if (d.getTime() < lo) d.setDate(d.getDate() + 1)
    for (let k = 0; d.getTime() < hi && k < 400; k++, d.setDate(d.getDate() + 1)) if (k % (step / DAY) === 0) out.push(d.getTime())
  }
  return out
}
/** a tick's label: the date at midnight, else the clock time */
export const tickLabel = (ts: number) => { const d = new Date(ts); return d.getHours() === 0 && d.getMinutes() === 0 ? `${d.getMonth() + 1}/${d.getDate()}` : `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}` }
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
export const fmtRange = (from: string, to: string) => dayRange(from, to)
/** a duration axis in one unit chosen by the largest value, so the ticks read “2 小时”, not “1 小时 57 分” */
export { durUnit }
