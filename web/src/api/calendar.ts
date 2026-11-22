/* Calendars as data sources (the Hub's steve-calendar, `/api/calendar/*`): the caller's calendar sources with
 * their calendars — whose each is, how much Steve keeps, on or off, how fresh — sync now, an .ics file for an ICS
 * source; and what is cached of them: the agenda over a range, one event with everyone in it, free time. Hubs without
 * it answer 404: the 接入 page shows no calendar section and the 日历 page its connect prompt. */
import { modReq, qs } from "@/api"
import type { Agenda, EventDetail, FreeTime } from "@/calendar/model"

export type CalSyncState = "new" | "ok" | "error"
export type CalCalendar = {
  id: string; connection_id: string; name: string; color: string | null
  kind: string; role: "free_busy" | "reader" | "writer" | "owner" | string; share: string
  /** the person whose calendar it is; `mine`: the caller's own */
  party_id: string | null; party_name: string | null; party_set: boolean; mine: boolean
  /** what Steve keeps: everything, or only busy times */
  detail: "full" | "busy"
  enabled: boolean; tz: string | null
  synced_at: number | null; sync_state: CalSyncState; sync_error: string | null; gone: boolean
}
export type CalSource = {
  connection_id: string; adapter: string; status: string; features: string[]; calendars: CalCalendar[]
  /** an ICS source: reads a subscription address, or the uploaded file (its name) */
  ics: { subscribed: boolean; file: string | null } | null
  report?: { changed: number; deleted: number; errors: [string, string][]; skipped: boolean }
}
export type CalEdit = { enabled?: boolean; party_id?: string | null; detail?: "full" | "busy" }

const enc = encodeURIComponent
export const calendarApi = {
  sources: () => modReq<CalSource[]>("GET", "/calendar/sources"),
  source: (conn: string) => modReq<CalSource>("GET", `/calendar/sources/${enc(conn)}`),
  sync: (conn: string) => modReq<CalSource>("POST", `/calendar/sources/${enc(conn)}/sync`),
  upload: (conn: string, name: string, text: string) => modReq<CalSource>("PUT", `/calendar/sources/${enc(conn)}/ics`, { name, text }),
  edit: (id: string, e: CalEdit) => modReq<CalCalendar>("PATCH", `/calendar/calendars/${enc(id)}`, e),
  /** the caller's own meetings over whole local days (`to` a date: the end of that day); none given: today */
  events: (q: { from?: string; to?: string } = {}) => modReq<Agenda>("GET", "/calendar/events" + qs(q)),
  event: (id: string) => modReq<EventDetail>("GET", `/calendar/events/${enc(id)}`),
  findTime: (b: { duration_min: number; people?: string[]; from?: string; to?: string }) => modReq<FreeTime>("POST", "/calendar/find-time", b),
}
