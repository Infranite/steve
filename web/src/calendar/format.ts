/* 日历's words for days and times, in the page's language. A day is a "YYYY-MM-DD" of the reader's zone and is named as
 * written (formatted at noon UTC in UTC), so the browser's own zone never shifts it. */
import { lang, t } from "@/i18n"
import { dateForName } from "./model"

const cache = new Map<string, Intl.DateTimeFormat>()
const fmt = (key: string, o: Intl.DateTimeFormatOptions) => {
  const k = lang() + key
  let f = cache.get(k)
  if (!f) { f = new Intl.DateTimeFormat(lang(), { ...o, timeZone: "UTC" }); cache.set(k, f) }
  return f
}
/** 11月18日 */
export const dayLabel = (day: string) => fmt("md", { month: "short", day: "numeric" }).format(dateForName(day))
/** 周三 */
export const weekdayLabel = (day: string) => fmt("wd", { weekday: "short" }).format(dateForName(day))
/** 11月18日 周三 */
export const dayLong = (day: string) => t("calendar:time.dayWeekday", { day: dayLabel(day), weekday: weekdayLabel(day) })
/** 2026年11月 */
export const monthLabel = (day: string) => fmt("ym", { year: "numeric", month: "long" }).format(dateForName(day))
/** the first day to the last, or one day */
export const daysLabel = (from: string, to: string) => (from === to ? dayLong(from) : t("calendar:time.range", { from: dayLabel(from), to: dayLabel(to) }))
/** 一 二 三 … for the month grid's head */
export const weekdayNarrow = (day: string) => fmt("wn", { weekday: "narrow" }).format(dateForName(day))

/** how far off a moment is, for a meeting: "18 分钟后", "2 小时后", "进行中", "已结束"; null beyond a day */
export function untilText(start: number, end: number, now: number): string | null {
  if (end <= now) return t("calendar:time.ended")
  if (start <= now) return t("calendar:time.started")
  const min = Math.ceil((start - now) / 60_000)
  if (min < 60) return t("calendar:time.inMin", { count: min })
  if (min < 24 * 60) return t("calendar:time.inHour", { count: Math.floor(min / 60) })
  return null
}
