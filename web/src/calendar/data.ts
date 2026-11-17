/* 日历's data: the caller's calendar sources (shared by the page's sources list and its colours), the agenda of the days
 * a view shows, today's agenda for the sidebar's next-meeting time, and the free time of a day. Everything reloads on
 * the `calendar` stream event (a sync wrote to a calendar, or its settings changed); the sources also on `connection`
 * (a source added or removed). A Hub without the calendar routes leaves them `unwired`. */
import { useEffect, useRef, useState, useSyncExternalStore } from "react"
import { isNotWired } from "@/api"
import { calendarApi, type CalSource } from "@/api/calendar"
import { t } from "@/i18n"
import { st, useHubEvents } from "@/store"
import { browserTz, dayIn, hmIn, isMeeting, rangeOf, type Agenda, type FreeTime, type View } from "./model"

/** the reader's zone as the Hub last answered it (every agenda carries it); the browser's until then */
let lastTz: string | null = null
export const knownTz = () => lastTz ?? browserTz()
const seen = (a: Agenda) => { lastTz = a.range.tz || lastTz; return a }

const failure = (e: unknown) => (e as Error).message || t("core:error.generic")

/* ───────── the sources: one list per workspace ───────── */
type Shared = { ws: string | null; sources: CalSource[] | null; error: string | null; unwired: boolean; ver: number }
const shared: Shared = { ws: null, sources: null, error: null, unwired: false, ver: 0 }
const subs = new Set<() => void>()
const bump = () => { shared.ver++; subs.forEach((f) => f()) }
let inflight: Promise<void> | null = null, again = false
export function loadSources(): Promise<void> {
  if (inflight) { again = true; return inflight }
  const ws = st.workspace?.id ?? null
  if (shared.ws !== ws) { shared.ws = ws; shared.sources = null; shared.error = null; shared.unwired = false; bump() }
  inflight = calendarApi.sources().then((r) => {
    if (ws !== (st.workspace?.id ?? null)) return
    shared.sources = r; shared.error = null; shared.unwired = false
  }, (e) => { shared.unwired = isNotWired(e); shared.error = failure(e) }).finally(() => {
    inflight = null; bump()
    if (again) { again = false; void loadSources() }
  })
  return inflight
}
/** the caller's calendar sources with their calendars; null until loaded */
export function useSources() {
  useSyncExternalStore((f) => { subs.add(f); return () => { subs.delete(f) } }, () => shared.ver)
  const ws = st.workspace?.id ?? null
  useEffect(() => { if (shared.ws !== ws || (!shared.sources && !inflight)) void loadSources() }, [ws])
  useHubEvents(["calendar", "connection"], () => void loadSources(), 300)
  return shared
}

/* ───────── the agenda of a view ───────── */
export type AgendaState = { agenda: Agenda | null; error: string | null; unwired: boolean; loading: boolean }
/** the events of the days `view` shows around `day`, in the reader's zone. The query is whole days, which the Hub reads
 *  in that zone; until its first answer the browser's zone stands in, and the page reads again if they differ. */
export function useAgenda(view: View, day: string | null, on = true): AgendaState & { reload: () => void } {
  const [s, setS] = useState<AgendaState>({ agenda: null, error: null, unwired: false, loading: true })
  const ws = st.workspace?.id ?? null
  const key = day ? `${ws}|${view}|${day}` : null
  const seq = useRef(0)
  const load = () => {
    if (!day || !on) return
    const n = ++seq.current
    setS((x) => ({ ...x, loading: true }))
    calendarApi.events(rangeOf(view, day)).then(seen).then((agenda) => { if (n === seq.current) setS({ agenda, error: null, unwired: false, loading: false }) },
      (e) => { if (n === seq.current) setS((x) => ({ agenda: x.agenda, error: failure(e), unwired: isNotWired(e), loading: false })) })
  }
  useEffect(() => { setS((x) => ({ ...x, agenda: x.agenda && key ? x.agenda : null })); load() }, [key, on]) // eslint-disable-line react-hooks/exhaustive-deps
  useHubEvents(["calendar"], load, 400)
  return { ...s, reload: load }
}

/** the free times of `day` the summary offers (the Hub's find-time: working hours, its grid, not before now) */
export function useFreeTime(day: string | null, minutes: number, watch: unknown) {
  const [free, setFree] = useState<FreeTime | null>(null)
  useEffect(() => {
    if (!day) return
    let live = true
    calendarApi.findTime({ duration_min: minutes, from: day, to: day }).then((r) => { if (live) setFree(r) }, () => { if (live) setFree(null) })
    return () => { live = false }
  }, [day, minutes, watch])
  return free
}

/* ───────── today, for the sidebar ───────── */
type Today = { ws: string | null; agenda: Agenda | null; ver: number }
const today: Today = { ws: null, agenda: null, ver: 0 }
const todaySubs = new Set<() => void>()
let todayFlight = false, todayDay: string | null = null
function loadToday(day: string) {
  if (todayFlight) return
  const ws = st.workspace?.id ?? null
  if (today.ws !== ws) { today.ws = ws; today.agenda = null }
  todayFlight = true; todayDay = day
  calendarApi.events().then(seen).then((a) => { if (ws === (st.workspace?.id ?? null)) today.agenda = a }, () => { today.agenda = null })
    .finally(() => { todayFlight = false; today.ver++; todaySubs.forEach((f) => f()) })
}
/** today's agenda (the Hub's today, in the reader's zone), loaded once per workspace and day and on `calendar` events;
 *  null on a Hub without calendars */
export function useTodayAgenda(day: string) {
  useSyncExternalStore((f) => { todaySubs.add(f); return () => { todaySubs.delete(f) } }, () => today.ver)
  const ws = st.workspace?.id ?? null
  useEffect(() => { if (today.ws !== ws || todayDay !== day) loadToday(day) }, [ws, day])
  useHubEvents(["calendar"], () => loadToday(day), 1000)
  return today.ws === ws ? today.agenda : null
}


/** the start of your next meeting today ("10:30"), for the sidebar's 日历 row; null when there is none or no calendar */
export function useNextMeetingAt(now: number): { at: string; title: string | null } | null {
  const tz = knownTz(), day = dayIn(now, tz)
  const a = useTodayAgenda(day)
  if (!a) return null
  const zone = a.range.tz || tz
  const e = a.events.filter((x) => isMeeting(x) && x.start_ms >= now && dayIn(x.start_ms, zone) === day).sort((x, y) => x.start_ms - y.start_ms)[0]
  return e ? { at: hmIn(e.start_ms, zone), title: e.title } : null
}
