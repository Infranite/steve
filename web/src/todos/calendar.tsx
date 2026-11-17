/* 日历, before any calendar is connected. A calendar is a data source like Feishu: each comes from a connector the
 * owner turns on under 接入, belongs to a person, and others' calendars show only free/busy unless shared. Until the
 * sync is built this page says what it will show and which sources are planned, links to 接入, and lists the deadlines
 * of your todos, which need no calendar. No sample data. */
import { CalendarDaysIcon, CalendarIcon, ListChecksIcon, PlugIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { useT } from "@/i18n"
import { useStore } from "@/store"
import { Blank, ConnectPrompt, go, Group, Loading, Note, Page, Panel, Pill, Row, Section, SectionLabel } from "@/ui"
import { useMinute, useTodos } from "./data"
import { upcoming } from "./model"
import { TodoRow } from "./parts"

/** the sources planned first, in the order they come (the calendar.v1 connector profile) */
const SOURCES = ["feishu", "invites", "caldav", "dingtalk", "exchange", "google", "mac"] as const
const WILL = ["views", "briefing", "prep", "invites", "others"] as const
/** how many days of deadlines the page lists */
const DAYS = 7

export function CalendarView() {
  useStore()
  const t = useT(), now = useMinute(), s = useTodos()
  const next = s.todos ? upcoming(s.todos, now, DAYS) : null
  return (
    <Page title={t("todos:calendar.title")} sub={t("todos:calendar.sub")} max={760}>
      <div className="flex flex-col gap-8" data-calendar="unwired">
        <ConnectPrompt icon={<CalendarIcon />} title={t("todos:calendar.none")} sub={t("todos:calendar.noneSub")}
          action={<Button onClick={() => go("#/integrations")} data-calendar-connect><PlugIcon />{t("todos:calendar.connect")}</Button>} data-calendar-prompt />
        <Section label={t("todos:calendar.deadlines", { count: DAYS })} right={<a href="#/todos" className="type-footnote text-muted-foreground hover:text-foreground">{t("todos:calendar.allTodos")}</a>} data-calendar-deadlines>
          {!next ? (s.error ? <Blank>{s.error}</Blank> : <Loading className="py-6" />) : next.length ? <div role="list" className="-mx-3 flex flex-col">{next.map((x) => <TodoRow key={x.id} x={x} now={now} />)}</div>
            : <Blank data-calendar-no-deadlines>{t("todos:calendar.noDeadlines", { count: DAYS })}</Blank>}
        </Section>
        <section data-calendar-sources>
          <SectionLabel>{t("todos:calendar.sourcesHead")}</SectionLabel>
          <Panel>{SOURCES.map((k) => <Row key={k} icon={<span className="flex size-8 items-center justify-center rounded-lg bg-muted text-muted-foreground" aria-hidden><CalendarDaysIcon className="size-4" strokeWidth={1.75} /></span>}
            title={t(`todos:calendar.source.${k}.name`)} sub={t(`todos:calendar.source.${k}.how`)} data-calendar-source={k}><Pill>{t("todos:calendar.planned")}</Pill></Row>)}</Panel>
          <Note>{t("todos:calendar.sourcesNote")}</Note>
        </section>
        <section data-calendar-will>
          <SectionLabel>{t("todos:calendar.willHead")}</SectionLabel>
          <Group>{WILL.map((k) => <Row key={k} icon={<ListChecksIcon className="size-4 text-muted-foreground" aria-hidden />} title={t(`todos:calendar.will.${k}.title`)} sub={t(`todos:calendar.will.${k}.sub`)} />)}</Group>
        </section>
      </div>
    </Page>
  )
}
