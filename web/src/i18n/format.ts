/* Locale-aware formatting: numbers, dates, relative times and durations in the current UI language.
 * The zh-CN output of every helper is exactly what the hand-written helpers printed before
 * (web/test/i18n.test.mjs pins it), so routing a helper through here changes nothing for zh-CN users. */
import { lang, t } from "./index.ts"

const MIN = 60_000, HOUR = 3_600_000, DAY = 86_400_000
const pad = (n: number) => String(n).padStart(2, "0")
const at = (v: number | Date) => (v instanceof Date ? v : new Date(v))
const startOfDay = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime()

/** a number in the UI language: num(12345) → "12,345" */
export const num = (n: number, opts?: Intl.NumberFormatOptions) => new Intl.NumberFormat(lang(), opts).format(n)
/** a date or time in the UI language with any Intl options */
export const date = (v: number | Date, opts?: Intl.DateTimeFormatOptions) => new Intl.DateTimeFormat(lang(), opts).format(at(v))

/** "08:30", 24-hour in every language */
export const hm = (v: number | Date) => { const d = at(v); return `${pad(d.getHours())}:${pad(d.getMinutes())}` }
/** "周五" / "Fri" */
export const weekday = (v: number | Date) => date(v, { weekday: "short" })
/** "11月5日" / "Nov 5" */
export const monthDay = (v: number | Date) => date(v, { month: "short", day: "numeric" })
/** "11-05" / "11/05": a compact date further away */
export const monthDayShort = (v: number | Date) => { const d = at(v); return t("common:time.monthDayShort", { mm: pad(d.getMonth() + 1), dd: pad(d.getDate()) }) }
/** "11月5日 14:30" / "Nov 5 14:30" */
export const monthDayTime = (v: number | Date) => t("common:time.dateAt", { date: monthDay(v), time: hm(v) })
/** a local calendar day "YYYY-MM-DD" (no time zone shift) as monthDay: "11月5日" */
export const dayName = (ymd: string) => { const [y, m, d] = ymd.split("-").map(Number); return monthDay(new Date(y, m - 1, d)) }
/** two days as one range: "11月1日 – 11月7日", one day as "11月5日" */
export const dayRange = (from: string, to: string) => (from === to ? dayName(from) : t("common:time.range", { from: dayName(from), to: dayName(to) }))

/** how long ago, rounded to one unit: "刚刚", "3 分钟前", "2 小时前", "4 天前".
 *  `justNow: false` says "1 分钟前" instead of "刚刚" under a minute. */
export function ago(ts: number, { now = Date.now(), justNow = true }: { now?: number; justNow?: boolean } = {}) {
  const m = Math.round((now - ts) / MIN)
  if (justNow && m < 1) return t("common:time.justNow")
  if (m < 60) return t("common:time.minutesAgo", { count: Math.max(m, 1) })
  if (m < 1440) return t("common:time.hoursAgo", { count: Math.round(m / 60) })
  return t("common:time.daysAgo", { count: Math.round(m / 1440) })
}

/** a past moment for a list: "刚刚", "14:30" today, "昨天 14:30", "周五" this week, else "11-05" */
export function pastTime(ts: number | null | undefined, now = Date.now()) {
  if (!ts) return ""
  if (now - ts < MIN) return t("common:time.justNow")
  const d = new Date(ts), days = Math.round((startOfDay(new Date(now)) - startOfDay(d)) / DAY)
  if (days === 0) return hm(d)
  if (days === 1) return t("common:time.yesterday", { time: hm(d) })
  if (days < 7) return weekday(d)
  return monthDayShort(d)
}

/** a moment near now, usually ahead: "今天 08:30", "明天 08:30", "昨天 08:30", "周五 09:00", else "11-12 09:00" */
export function nearTime(ts: number | null | undefined, now = Date.now()) {
  if (!ts) return ""
  const d = new Date(ts), days = Math.round((startOfDay(d) - startOfDay(new Date(now))) / DAY), time = hm(d)
  if (days === 0) return t("common:time.today", { time })
  if (days === 1) return t("common:time.tomorrow", { time })
  if (days === -1) return t("common:time.yesterday", { time })
  if (days > 1 && days < 7) return t("common:time.weekdayAt", { weekday: weekday(d), time })
  return t("common:time.dateAt", { date: monthDayShort(d), time })
}

/** a length of time in one rounded unit, at least a minute: "3 分钟", "2 小时", "4 天" */
export function span(ms: number) {
  const m = Math.max(1, Math.round(ms / MIN))
  if (m < 60) return t("common:dur.minutes", { count: m })
  if (m < 1440) return t("common:dur.hours", { count: Math.round(m / 60) })
  return t("common:dur.days", { count: Math.round(m / 1440) })
}

/** how long something has been going, to the minute: "刚开始", "12 分钟", "2 小时 5 分" */
export function elapsed(ms: number) {
  const m = Math.max(0, Math.round(ms / MIN))
  if (m < 1) return t("common:time.justStarted")
  if (m < 60) return t("common:dur.minutes", { count: m })
  return t("common:dur.hourMin", { h: Math.floor(m / 60), m: m % 60 })
}

/** a run's length, to the second: "42 秒", "3 分 5 秒", "1 小时 2 分" */
export function clockDur(ms: number) {
  const s = Math.max(0, Math.round(ms / 1000))
  if (s < 60) return t("common:dur.seconds", { count: s })
  if (s < 3600) return t("common:dur.minSec", { m: Math.floor(s / 60), s: s % 60 })
  return t("common:dur.hourMin", { h: Math.floor(s / 3600), m: Math.floor((s % 3600) / 60) })
}

/** a long length of time in at most two units: "42 秒", "12 分钟", "3 小时", "3 小时 12 分", "2 天 4 小时" */
export function longDur(ms: number) {
  const m = Math.round(ms / MIN)
  if (ms < MIN) return t("common:dur.seconds", { count: Math.max(0, Math.round(ms / 1000)) })
  if (m < 60) return t("common:dur.minutes", { count: m })
  const h = Math.floor(m / 60)
  if (h < 48) return m % 60 ? t("common:dur.hourMin", { h, m: m % 60 }) : t("common:dur.hours", { count: h })
  const d = Math.floor(h / 24)
  return h % 24 ? t("common:dur.dayHour", { d, h: h % 24 }) : t("common:dur.days", { count: d })
}

/** a short machine time: "<1 ms", "240 ms", "1.5 秒" */
export function msDur(ms: number) {
  if (ms < 1) return t("common:time.lessThanMs")
  if (ms < 1000) return t("common:time.ms", { n: Math.round(ms) })
  return t("common:time.secondsN", { n: (ms / 1000).toFixed(1) })
}

/** the one unit a duration axis is drawn in, chosen by its largest value: ["小时", 3600000] */
export const durUnit = (maxMs: number): [string, number] =>
  maxMs >= 2 * DAY ? [t("common:dur.unitDays"), DAY] : maxMs >= 2 * HOUR ? [t("common:dur.unitHours"), HOUR] : [t("common:dur.unitMinutes"), MIN]
