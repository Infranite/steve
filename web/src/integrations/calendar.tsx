/* The calendar part of a connection on the 接入 page: a calendar source (an ICS subscription or file, the demo
 * calendar, a connector plugin that declares calendar.v1) lists its calendars here. For each: on or off, whose it
 * is (yours, or a contact's), how much Steve keeps (everything, or only busy times — the default for someone
 * else's), and how fresh it is ("数据截至 hh:mm", the error when the last sync failed). Sync now; an ICS source
 * without a subscription address takes an .ics file. Renders nothing for a connection that is no calendar source. */
import { useEffect, useRef, useState } from "react"
import { CalendarIcon, CheckIcon, RefreshCwIcon, UploadIcon, UserRoundIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Switch } from "@/components/ui/switch"
import { Spinner } from "@/components/ui/spinner"
import { CommandMenu } from "@/components/affect/command-menu-clean"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { Group, Row, SectionLabel } from "@/ui"
import { Select } from "@/view-shared"
import { useT } from "@/i18n"
import { ago } from "@/i18n/format"
import { toast } from "@/store"
import { useBusy } from "@/accounts/load"
import { useHubQuery } from "@/data/use-hub-query"
import { people, type ContactBrief } from "@/api/people"
import { calendarApi, type CalCalendar, type CalSource } from "@/api/calendar"

/** how fresh a calendar is, in one line */
function freshness(c: CalCalendar, t: ReturnType<typeof useT>) {
  const asOf = c.synced_at ? ago(c.synced_at) : null
  if (!c.enabled) return t("integrations:calendar.off")
  if (c.sync_state === "error") return asOf ? t("integrations:calendar.failedAsOf", { error: c.sync_error ?? "", asOf }) : t("integrations:calendar.failed", { error: c.sync_error ?? "" })
  if (!asOf) return t("integrations:calendar.notYet")
  return t("integrations:calendar.asOf", { asOf })
}

/** whose calendar it is: you, or a contact */
function PersonPick({ c, onPick, disabled }: { c: CalCalendar; onPick: (party: string | null) => void; disabled?: boolean }) {
  const t = useT()
  const [open, setOpen] = useState(false), [q, setQ] = useState(""), [found, setFound] = useState<ContactBrief[] | null>(null)
  useEffect(() => {
    if (!open) return
    let live = true
    const h = setTimeout(() => { people.contacts({ q: q.trim() || undefined, limit: 30 }).then((xs) => { if (live) setFound(xs) }, () => { if (live) setFound([]) }) }, q ? 150 : 0)
    return () => { live = false; clearTimeout(h) }
  }, [open, q])
  const label = c.mine ? t("integrations:calendar.mine") : c.party_name ?? t("integrations:calendar.nobody")
  const xs = (found ?? []).filter((p) => p.kind !== "bot" && !p.is_me)
  return (
    <Popover open={open} onOpenChange={(o) => { setOpen(o); if (!o) setQ("") }}>
      <PopoverTrigger render={<Button variant="outline" size="xs" disabled={disabled} className="h-6 max-w-[200px] gap-1 rounded-full px-2 font-normal" aria-label={t("integrations:calendar.whoseAria", { name: c.name, who: label })} data-calendar-person={c.id} />}>
        <UserRoundIcon className="size-3.5 shrink-0 text-muted-foreground" /><span className="truncate">{label}</span>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-[min(300px,calc(100vw-24px))] gap-0 rounded-2xl p-0">
        <div className="border-b px-3 py-2 type-footnote text-muted-foreground">{t("integrations:calendar.whoseOf", { name: c.name })}</div>
        <CommandMenu
          bare
          autoFocus
          showFooter={false}
          shouldFilter={false}
          onQueryChange={setQ}
          onClose={() => setOpen(false)}
          labels={{ placeholder: t("integrations:calendar.search"), empty: found ? t("integrations:calendar.noMatch") : t("integrations:calendar.loading") }}
          groups={[{
            id: "people",
            label: "",
            commands: [
              { id: "__me", label: t("integrations:calendar.mine"), trailing: c.mine ? <CheckIcon className="size-4" /> : undefined, data: { "data-calendar-person-opt": "me" } },
              ...xs.map((p) => ({
                id: p.id,
                label: p.name,
                trailing: !c.mine && c.party_id === p.id ? <CheckIcon className="size-4" /> : undefined,
                data: { "data-calendar-person-opt": p.name },
              })),
            ],
          }]}
          onRun={(id) => {
            setOpen(false)
            if (id === "__me") { if (!c.mine) onPick(null) }
            else if (c.party_id !== id) onPick(id)
          }}
        />
      </PopoverContent>
    </Popover>
  )
}

function CalendarRow({ c, onChange }: { c: CalCalendar; onChange: (x: CalCalendar) => void }) {
  const t = useT()
  const [busy, act] = useBusy()
  const edit = async (e: Parameters<typeof calendarApi.edit>[1], ok?: string) => { const x = await act(() => calendarApi.edit(c.id, e), ok); if (x) onChange(x) }
  const busyOnly = c.role === "free_busy"
  return (
    <Row title={<span className="inline-flex min-w-0 items-center gap-2"><span className="size-2.5 shrink-0 rounded-full" style={{ background: c.color ?? "var(--muted-foreground)" }} aria-hidden /><span className="truncate">{c.name}</span></span>}
      sub={<span className="flex flex-col gap-1.5">
        <span className={c.enabled && c.sync_state === "error" ? "text-attention" : undefined} data-calendar-state={c.sync_state}>{freshness(c, t)}</span>
        {c.enabled && <span className="flex flex-wrap items-center gap-1.5">
          <PersonPick c={c} disabled={busy} onPick={(party) => void edit({ party_id: party }, party ? t("integrations:calendar.boundOther") : t("integrations:calendar.boundMine"))} />
          <Select value={c.detail} testId={"calendar-detail-" + c.id} disabled={busy} label={t("integrations:calendar.detailAria", { name: c.name })}
            onChange={(v) => void edit({ detail: v as "full" | "busy" })}
            options={[{ v: "full", l: t("integrations:calendar.detailFull"), disabled: busyOnly }, { v: "busy", l: t("integrations:calendar.detailBusy") }]} />
          {busy && <Spinner className="size-3" />}
        </span>}
      </span>}
      data-calendar={c.id} data-calendar-detail={c.detail} data-calendar-mine={c.mine}>
      <Switch checked={c.enabled} disabled={busy} onCheckedChange={(on) => void edit({ enabled: on })} aria-label={t("integrations:calendar.enableAria", { name: c.name })} />
    </Row>
  )
}

/** a connection's calendars; nothing when it is no calendar source */
export function CalendarSection({ conn }: { conn: string }) {
  const t = useT()
  const L = useHubQuery(["calendar", "source", conn], () => calendarApi.source(conn), { events: ["calendar"] })
  const [x, setX] = useState<CalSource | null>(null)
  const [busy, act] = useBusy()
  const file = useRef<HTMLInputElement>(null)
  useEffect(() => { if (L.data !== undefined) setX(L.data) }, [L.data])
  if (L.error) return <p role="alert" className="type-callout text-destructive">{L.error.message}</p>
  if (!x) return null
  const cals = x.calendars.filter((c) => !c.gone)
  const needsFile = !!x.ics && !x.ics.subscribed
  const sync = async () => {
    const r = await act(() => calendarApi.sync(conn))
    if (!r) return
    setX(r)
    toast(r.report?.skipped ? t("integrations:calendar.justSynced") : r.report?.errors.length ? t("integrations:calendar.syncFailed") : t("integrations:calendar.synced"))
  }
  const upload = async (f: File) => {
    const text = await f.text()
    const r = await act(() => calendarApi.upload(conn, f.name, text), t("integrations:calendar.uploaded", { name: f.name }))
    if (r) setX(r)
  }
  return (
    <section data-calendar-section={conn}>
      <SectionLabel><span className="inline-flex items-center gap-1.5"><CalendarIcon className="size-3.5" aria-hidden />{t("integrations:calendar.title")}</span></SectionLabel>
      {cals.length > 0
        ? <Group>{cals.map((c) => <CalendarRow key={c.id} c={c} onChange={(nc) => setX({ ...x, calendars: x.calendars.map((y) => (y.id === nc.id ? nc : y)) })} />)}</Group>
        : <div className="px-1 type-callout text-muted-foreground" data-calendar-empty>{needsFile ? t("integrations:calendar.noFile") : t("integrations:calendar.none")}</div>}
      <div className="mt-2 flex flex-wrap items-center gap-2">
        <Button size="sm" variant="outline" disabled={busy} onClick={() => void sync()} data-calendar-sync><RefreshCwIcon />{t("integrations:calendar.syncNow")}</Button>
        {needsFile && <>
          <input ref={file} type="file" accept=".ics,text/calendar" className="hidden" onChange={(e) => { const f = e.target.files?.[0]; e.target.value = ""; if (f) void upload(f) }} data-calendar-file-input />
          <Button size="sm" variant="outline" disabled={busy} onClick={() => file.current?.click()} data-calendar-upload><UploadIcon />{x.ics?.file ? t("integrations:calendar.replaceFile") : t("integrations:calendar.uploadFile")}</Button>
          {x.ics?.file && <span className="type-footnote text-muted-foreground" data-calendar-file>{x.ics.file}</span>}
        </>}
        {busy && <Spinner className="size-3" />}
      </div>
      <div className="mt-2 px-1 type-footnote text-muted-foreground">{t("integrations:calendar.note")}</div>
    </section>
  )
}
