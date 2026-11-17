/* 日历 before any calendar source is connected (or on a Hub without calendars). A calendar is a data source like
 * Feishu: each comes from a connector the owner turns on under 接入, belongs to a person, and others' calendars give
 * free/busy only unless shared. The page says so, links to 接入, lists the sources (an ICS subscription or file works
 * today; the others are planned) and the deadlines of your todos, which need no calendar. No sample data. */
import { CalendarDaysIcon, CalendarIcon, PlugIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { useT } from "@/i18n"
import { useMinute, useTodos } from "@/todos/data"
import { upcoming } from "@/todos/model"
import { TodoRow } from "@/todos/parts"
import { Blank, ConnectPrompt, go, Loading, Note, Panel, Pill, Row, Section, SectionLabel } from "@/ui"

/** the sources in the order they come (the calendar.v1 connector profile); `ics` works today */
const SOURCES = ["ics", "feishu", "invites", "caldav", "dingtalk", "exchange", "google", "mac"] as const
const READY = new Set<string>(["ics"])
/** how many days of deadlines the page lists */
const DAYS = 7

export function Unconnected({ unwired }: { unwired: boolean }) {
  const t = useT(), now = useMinute(), s = useTodos()
  const next = s.todos ? upcoming(s.todos, now, DAYS) : null
  return (
    <div className="flex flex-col gap-8" data-calendar="unwired">
      <ConnectPrompt icon={<CalendarIcon />} title={t(unwired ? "calendar:none.unwired" : "calendar:none.title")} sub={t(unwired ? "calendar:none.unwiredSub" : "calendar:none.sub")}
        action={!unwired && <Button onClick={() => go("#/integrations/ics")} data-calendar-connect><PlugIcon />{t("calendar:none.connect")}</Button>} data-calendar-prompt />
      <Section label={t("calendar:none.deadlines", { count: DAYS })} right={<a href="#/todos" className="type-footnote text-muted-foreground hover:text-foreground">{t("calendar:due.all")}</a>} data-calendar-deadlines>
        {!next ? (s.error ? <Blank>{s.error}</Blank> : <Loading className="py-6" />) : next.length ? <div role="list" className="-mx-3 flex flex-col">{next.map((x) => <TodoRow key={x.id} x={x} now={now} />)}</div>
          : <Blank data-calendar-no-deadlines>{t("calendar:none.noDeadlines", { count: DAYS })}</Blank>}
      </Section>
      <section data-calendar-sources>
        <SectionLabel>{t("calendar:none.sourcesHead")}</SectionLabel>
        <Panel>{SOURCES.map((k) => <Row key={k} icon={<span className="flex size-8 items-center justify-center rounded-lg bg-muted text-muted-foreground" aria-hidden><CalendarDaysIcon className="size-4" strokeWidth={1.75} /></span>}
          title={t(`calendar:none.source.${k}.name`)} sub={t(`calendar:none.source.${k}.how`)} onClick={READY.has(k) && !unwired ? () => go("#/integrations/" + k) : undefined} data-calendar-source={k}>
          <Pill tone={READY.has(k) ? "success" : "neutral"}>{t(READY.has(k) ? "calendar:none.ready" : "calendar:none.planned")}</Pill></Row>)}</Panel>
        <Note>{t("calendar:none.note")}</Note>
      </section>
    </div>
  )
}
