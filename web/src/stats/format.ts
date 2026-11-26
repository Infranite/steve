/* 统计页的数字格式（stats.md §6.2）：大数缩写、美元两位（不到 1 分写“<$0.01”）、时长“3 小时 12 分”。 */
import type { Filter, Usage } from "@/api/stats"
import { t } from "@/i18n"
import { coverageValue } from "./usage-coverage"
import { dayRange, durUnit, longDur, monthDayTime } from "@/i18n/format"

export const fmtN = (n: number | null | undefined) => {
  if (n == null || !isFinite(n)) return "—"
  const a = Math.abs(n)
  if (a >= 1e9) return (n / 1e9).toFixed(a >= 1e10 ? 0 : 1).replace(/\.0$/, "") + "B"
  if (a >= 1e6) return (n / 1e6).toFixed(a >= 1e7 ? 0 : 1).replace(/\.0$/, "") + "M"
  if (a >= 1e3) return (n / 1e3).toFixed(a >= 1e4 ? 0 : 1).replace(/\.0$/, "") + "K"
  return String(Math.round(n * 10) / 10)
}
/** The same presentation for session, item and statistics usage; unknown never formats as zero. */
export const fmtUsage = (u: Pick<Usage, "total" | "coverage"> & Partial<Pick<Usage,"input" | "output" | "cache_read" | "cache_write">>, field: "total" | "input" | "output" | "cache_read" | "cache_write" = "total") =>
  coverageValue(u, field, fmtN, { not_reported: t("stats:coverage.not_reported"), unavailable: t("stats:coverage.unavailable"), partial: t("stats:coverage.partial") })

export const fmtUsd = (n: number | null | undefined) => (n == null || !isFinite(n) ? "—" : n === 0 ? "$0" : n < 0.01 ? "<$0.01" : "$" + (n >= 1000 ? Math.round(n).toLocaleString("en-US") : n.toFixed(2)))
const KIB = 1024, MIB = KIB * 1024, GIB = MIB * 1024, TIB = GIB * 1024
/** Human-readable binary byte count shared across stats, Node vitals, and session rows. */
export const fmtBytes = (bytes: number | null | undefined) => {
  if (bytes == null || !isFinite(bytes) || bytes < 0) return "—"
  if (bytes >= TIB) return (bytes / TIB).toFixed(1).replace(/\.0$/, "") + " TB"
  if (bytes >= GIB) return (bytes / GIB).toFixed(1).replace(/\.0$/, "") + " GB"
  if (bytes >= MIB) return (bytes / MIB).toFixed(1).replace(/\.0$/, "") + " MB"
  if (bytes >= KIB) return (bytes / KIB).toFixed(1).replace(/\.0$/, "") + " KB"
  return `${Math.round(bytes)} B`
}
/** An estimate that leaves `unpriced` tokens out: “≥$1.20 · 7.4M 未定价”, “— · 7.4M 未定价” when what is priced
 * rounds to nothing (not “<$0.01” for millions of tokens); plain [`fmtUsd`] when every token is priced. */
export const fmtEst = (est: number, unpriced = 0) =>
  !unpriced ? fmtUsd(est) : t("stats:word.estUnpriced", { usd: est >= 0.01 ? "≥" + fmtUsd(est) : "—", n: fmtN(unpriced) })
/** Preserve unknown cost and coverage alongside the estimate, including an observed zero. */
export const fmtUsageCost = (u: Pick<Usage,"total" | "est" | "reported" | "unpriced_tokens" | "coverage">) => {
  const known = u.coverage.total != null
  if (!known) return u.coverage.cost_reported != null ? t("stats:word.adapterQuote", { usd: fmtUsd(u.coverage.cost_reported) }) : u.reported > 0 ? t("stats:word.adapterQuoteFloor", { usd: fmtUsd(u.reported) }) : fmtUsage(u)
  const value = fmtEst(u.est, u.unpriced_tokens)
  return u.coverage.state === "partial" ? `${!u.unpriced_tokens ? "≥" : ""}${value} · ${t("stats:coverage.partial")}` : value
}
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
/** the range a chart covers, said the same way on every chart: “11月1日 – 11月7日”, one day as “11月5日” */
export const fmtRange = (from: string, to: string) => dayRange(from, to)
/** a duration axis in one unit chosen by the largest value, so the ticks read “2 小时”, not “1 小时 57 分” */
export { durUnit }
