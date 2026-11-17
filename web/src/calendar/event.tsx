/* One event, read on its own (GET /calendar/events/{id}): when, which calendar and where else the same meeting is,
 * the place and the meeting link, who organizes it, everyone in it with their reply, its notes, and how fresh the copy
 * is. A calendar that keeps busy times only says so instead of a title. Read only: changing an event, adding people
 * and answering an invitation come with the write-back step, and a change other people are told about asks first. */
import { useEffect, useState } from "react"
import { CalendarIcon, ClockIcon, LockIcon, MapPinIcon, UserRoundIcon, VideoIcon } from "lucide-react"
import { cn } from "cn"
import { HubError } from "@/api"
import { calendarApi } from "@/api/calendar"
import { Button } from "@/components/ui/button"
import { useT } from "@/i18n"
import { useHubEvents } from "@/store"
import { Blank, Loading, Notice } from "@/ui"
import { daysLabel, dayLong, untilText } from "./format"
import { dayIn, hmIn, safeUrl, type Attendee, type EventDetail } from "./model"
import { useTitle, type Colors } from "./parts"

/** the event `id`, loaded and kept fresh on calendar events */
function useEvent(id: string) {
  const [s, setS] = useState<{ e: EventDetail | null; err: string | null; missing: boolean }>({ e: null, err: null, missing: false })
  const load = () => calendarApi.event(id).then((e) => setS({ e, err: null, missing: false }), (x) => setS((p) => ({ e: p.e, err: (x as Error).message, missing: x instanceof HubError && x.status === 404 })))
  useEffect(() => { setS({ e: null, err: null, missing: false }); void load() }, [id]) // eslint-disable-line react-hooks/exhaustive-deps
  useHubEvents(["calendar"], () => void load(), 400)
  return s
}

const RSVP = ["accepted", "tentative", "declined", "needs_action"] as const
const rsvpKey = (r: string | null | undefined) => (RSVP as readonly string[]).includes(r ?? "") ? r as (typeof RSVP)[number] : "none"
const RSVP_TONE: Record<string, string> = { accepted: "text-success", tentative: "text-attention", declined: "text-destructive", needs_action: "text-muted-foreground", none: "text-muted-foreground" }

function Person({ a }: { a: Attendee }) {
  const t = useT()
  const name = a.name || a.email || (a.is_self ? t("calendar:detail.tag.me") : a.ext_ref || "?")
  const k = rsvpKey(a.rsvp)
  const body = <>
    <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-primary/12 type-label font-semibold text-primary" aria-hidden>{Array.from(name)[0]?.toUpperCase()}</span>
    <span className="min-w-0 flex-1 truncate type-callout" title={a.email ?? undefined}>{name}</span>
    {a.is_self && name !== t("calendar:detail.tag.me") && <span className="type-label text-tertiary">{t("calendar:detail.tag.me")}</span>}
    {a.organizer && <span className="type-label text-tertiary">{t("calendar:detail.tag.organizer")}</span>}
    {a.optional && <span className="type-label text-tertiary">{t("calendar:detail.tag.optional")}</span>}
    <span className={cn("shrink-0 type-footnote", RSVP_TONE[k])} data-cal-rsvp={k}>{t(`calendar:detail.rsvp.${k}`)}</span>
  </>
  return a.party_id && !a.is_self
    ? <li><a href={"#/contacts/" + a.party_id} className="-mx-1.5 flex min-h-9 items-center gap-2.5 rounded-md px-1.5 hover:bg-accent" data-cal-person={name}>{body}</a></li>
    : <li className="flex min-h-9 items-center gap-2.5" data-cal-person={name}>{body}</li>
}

function Fact({ icon, children, k }: { icon: React.ReactNode; children: React.ReactNode; k: string }) {
  return <div className="flex items-start gap-2.5 type-callout" data-cal-fact={k}><span className="mt-0.5 shrink-0 text-muted-foreground [&>svg]:size-4" aria-hidden>{icon}</span><div className="min-w-0 flex-1">{children}</div></div>
}

/** The event's details, for the panel beside the grid and for a phone's page */
export function EventBody({ id, tz, now, colors }: { id: string; tz: string; now: number; colors: Colors }) {
  const t = useT(), title = useTitle()
  const { e, err, missing } = useEvent(id)
  if (!e) return err ? <Blank data-cal-event-missing>{missing ? t("calendar:detail.missing") : err}</Blank> : <Loading className="py-10" />
  const color = colors.get(e.calendar.id) ?? "var(--series-other)", url = safeUrl(e.meeting_url)
  const until = e.all_day ? null : untilText(e.start_ms, e.end_ms, now)
  const sameDay = e.all_day || dayIn(e.start_ms, tz) === dayIn(e.end_ms - 1, tz)
  const when = e.all_day ? daysLabel(e.start, e.end ?? e.start)
    : sameDay ? t("calendar:detail.when", { day: dayLong(dayIn(e.start_ms, tz)), from: hmIn(e.start_ms, tz), to: hmIn(e.end_ms, tz) })
    : t("calendar:detail.whenLong", { from: `${dayLong(dayIn(e.start_ms, tz))} ${hmIn(e.start_ms, tz)}`, to: `${dayLong(dayIn(e.end_ms - 1, tz))} ${hmIn(e.end_ms, tz)}` })
  const others = e.attendees.filter((a) => !a.is_self).length
  return (
    <div className="flex flex-col gap-5" data-cal-detail={e.id}>
      <div>
        <h2 className={cn("type-title", e.my_rsvp === "declined" && "line-through decoration-tertiary")} data-cal-detail-title>{title(e)}</h2>
        {e.deleted ? <Notice tone="destructive" className="mt-2">{t("calendar:detail.deleted")}</Notice> : e.status === "cancelled" ? <Notice tone="destructive" className="mt-2">{t("calendar:detail.cancelled")}</Notice> : null}
      </div>
      <div className="flex flex-col gap-2.5">
        <Fact k="when" icon={<ClockIcon />}>{when}{e.all_day && <span className="text-muted-foreground"> · {t("calendar:grid.allDay")}</span>}{until && <span className="text-primary"> · {until}</span>}</Fact>
        <Fact k="calendar" icon={<CalendarIcon />}>
          <span className="inline-flex items-center gap-1.5"><span className="size-2 rounded-full" style={{ backgroundColor: color }} aria-hidden />{e.calendar.name}</span>
          {e.organizer?.me ? <span className="text-muted-foreground"> · {t("calendar:detail.organizerMe")}</span> : e.organizer ? <span className="text-muted-foreground"> · {t("calendar:detail.organizer", { name: e.organizer.name || e.organizer.email })}</span> : null}
          {e.also_in?.length ? <div className="type-footnote text-muted-foreground" data-cal-also>{t("calendar:detail.alsoIn", { list: e.also_in.join(t("calendar:sum.sep")) })}</div> : null}
        </Fact>
        {e.location && <Fact k="where" icon={<MapPinIcon />}><span className="wrap-anywhere">{e.location}</span></Fact>}
        {url && <Fact k="link" icon={<VideoIcon />}><div className="flex min-w-0 items-center gap-2"><span className="min-w-0 flex-1 truncate text-muted-foreground" title={url}>{url.replace(/^https?:\/\//i, "")}</span>
          <Button size="xs" nativeButton={false} render={<a href={url} target="_blank" rel="noopener noreferrer" />} data-cal-detail-join>{t("calendar:detail.join")}</Button></div></Fact>}
        {e.my_rsvp && <Fact k="reply" icon={<UserRoundIcon />}>{t("calendar:detail.reply")}<span className={cn("ml-1.5", RSVP_TONE[rsvpKey(e.my_rsvp)])}>{t(`calendar:detail.rsvp.${rsvpKey(e.my_rsvp)}`)}</span></Fact>}
        {e.busy_only && <Fact k="busy" icon={<LockIcon />}><span className="text-muted-foreground">{t("calendar:detail.busyOnly")}</span></Fact>}
      </div>
      {e.attendees.length > 0 && (
        <section data-cal-people>
          <div className="mb-1 type-label text-tertiary">{t("calendar:detail.people", { count: others + 1 })}</div>
          <ul className="flex flex-col">{e.attendees.map((a, i) => <Person key={(a.party_id ?? a.email ?? a.name ?? "") + i} a={a} />)}</ul>
        </section>
      )}
      {e.excerpt && (
        <section data-cal-notes>
          <div className="mb-1 type-label text-tertiary">{t("calendar:detail.notes")}</div>
          <p className="max-h-60 overflow-y-auto whitespace-pre-wrap wrap-anywhere type-callout text-foreground/85">{e.excerpt}</p>
        </section>
      )}
      <div className="flex flex-col gap-1 border-t pt-3 type-footnote text-muted-foreground">
        {e.as_of && <span data-cal-as-of>{t("calendar:detail.asOf", { when: e.as_of.startsWith(dayIn(now, tz)) ? e.as_of.slice(11) : e.as_of })}</span>}
        <span>{t("calendar:detail.readOnly")}</span>
      </div>
    </div>
  )
}
