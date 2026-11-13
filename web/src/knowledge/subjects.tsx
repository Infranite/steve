/* 对象: what Steve knows about one person, project or bot, and where it learned it. The tab lists every subject
 * with how much is known; a subject's page (`#/knowledge/about/<type>:<id>`) gives its memories (each with its
 * source), the documents about it, where it learned it, and where that knowledge was used lately. Lists, not a
 * graph: the question is "what and from where", which a list answers. */
import { useCallback, useEffect, useState } from "react"
import { BotIcon, FileTextIcon, FolderIcon, MessagesSquareIcon, SearchIcon, UserIcon, UsersIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Page, Panel, Row, Section, Empty, Meta, Pill, Tile, Chevron, Loading, go } from "@/ui"
import { useHubEvents, tm } from "@/store"
import { kb, levelLabel, LONG_TERM, type Mem, type Subject, type SubjectDetail } from "@/api/knowledge"
import { errMsg } from "@/knowledge/fields"
import { OriginLine, originLabel, savedLine, subjectKind, subjectHref } from "@/knowledge/origin"
import { useT } from "@/i18n"

const ICON: Record<string, typeof UserIcon> = { party: UserIcon, project: FolderIcon, bot: BotIcon, item: FileTextIcon, space: UsersIcon, persona: UserIcon, me: UserIcon }
const docLink = (d: { id: string; source: { kind: string } }) => (d.source.kind === "notes" ? "#/knowledge/note/" : "#/knowledge/doc/") + d.id

/** the 对象 tab: every subject, most known first */
export function SubjectsTab() {
  const t = useT()
  const [xs, setXs] = useState<Subject[] | null>(null), [err, setErr] = useState("")
  const load = useCallback(() => kb.subjects().then(setXs, (e) => setErr(errMsg(e))), [])
  useEffect(() => { void load() }, [load])
  useHubEvents(["mm", "kb"], () => void load())
  if (err) return <Empty title={t("knowledge:subject.failed")} sub={err} />
  if (!xs) return <Loading className="py-12" />
  if (!xs.length) return <Empty title={t("knowledge:subject.noneTitle")} sub={t("knowledge:subject.noneSub")} />
  return (
    <div className="flex flex-col gap-3">
      <Panel data-subjects>{xs.map((s) => (
        <Row key={s.type + s.id} onClick={() => go(subjectHref(s))} data-subject={s.id} icon={<Tile I={ICON[s.type] ?? UserIcon} />}
          title={<span className="truncate">{s.name || s.id}</span>}
          sub={<Meta inline><span>{subjectKind(s.type)}</span>{s.mems > 0 && <span>{t("knowledge:subject.mems", { count: s.mems })}</span>}{s.docs > 0 && <span>{t("knowledge:subject.docs", { count: s.docs })}</span>}{s.candidates > 0 && <span className="text-attention">{t("knowledge:subject.pending", { count: s.candidates })}</span>}{s.last > 0 && <span>{t("knowledge:subject.updated", { time: tm(s.last) })}</span>}</Meta>}>
          <Chevron />
        </Row>))}</Panel>
      <div className="px-1 type-footnote text-muted-foreground">{t("knowledge:subject.footer")}</div>
    </div>
  )
}

/** one memory as a row: its text, where it came from and when, how often it was used */
function MemRow({ m }: { m: Mem }) {
  const t = useT()
  return (
    <Row onClick={() => go("#/knowledge?mem=" + encodeURIComponent(m.id))} data-subject-mem={m.id}
      title={<span className="inline-flex flex-wrap items-baseline gap-x-2 gap-y-1"><span className="min-w-0 break-words">{m.text}</span>{m.status === "candidate" && <Pill tone="attention">{t("knowledge:mems.pending")}</Pill>}{m.status === "stale" && <Pill>{t("knowledge:mems.stale")}</Pill>}</span>}
      sub={<Meta inline><span className="min-w-0 truncate"><OriginLine m={m} about={false} /></span>{savedLine(m).split(" · ").map((x) => <span key={x}>{x}</span>)}<span>{levelLabel(m.level)}</span></Meta>} />
  )
}

/** where it learned it: how many memories each source gave */
function Learned({ mems }: { mems: Mem[] }) {
  const t = useT()
  const by = new Map<string, { label: string; href: string; n: number }>()
  for (const m of mems) {
    const label = originLabel(m), k = (m.origin?.kind ?? "you") + label, x = by.get(k)
    if (x) x.n++; else by.set(k, { label, href: m.origin?.href ?? "", n: 1 })
  }
  const xs = [...by.values()].sort((a, b) => b.n - a.n)
  if (!xs.length) return null
  return (
    <Section label={t("knowledge:subject.learned")} data-subject-learned>
      <div className="flex flex-wrap gap-1.5">{xs.map((x) => {
        const body = <><span className="min-w-0 truncate">{x.label}</span><span className="tabular-nums text-muted-foreground">{x.n}</span></>
        const cls = "inline-flex h-7 max-w-full items-center gap-1.5 rounded-lg bg-muted/70 px-2.5 type-footnote text-foreground/90"
        return x.href ? <a key={x.label} href={x.href} className={cls + " transition-colors hover:bg-accent"} data-learned={x.label}>{body}</a> : <span key={x.label} className={cls} data-learned={x.label}>{body}</span>
      })}</div>
    </Section>
  )
}

const USE_ICON: Record<string, typeof UserIcon> = { session: BotIcon, chat: MessagesSquareIcon, search: SearchIcon }

/** `#/knowledge/about/<type>:<id>` */
export function SubjectView({ type, id }: { type: string; id: string }) {
  const t = useT()
  const [d, setD] = useState<SubjectDetail | null>(null), [err, setErr] = useState("")
  const load = useCallback(() => kb.subject(type, id).then(setD, (e) => setErr(errMsg(e))), [type, id])
  useEffect(() => { void load() }, [load])
  useHubEvents(["mm", "kb"], () => void load())
  const back: [string, string] = ["#/knowledge?tab=subjects", t("knowledge:page.title")]
  if (err) return <Page title={t("knowledge:subject.title")} back={back}><Empty title={t("knowledge:subject.openFailed")} sub={err} /></Page>
  if (!d) return <Page title={t("knowledge:subject.title")} back={back}><Loading /></Page>
  const long = d.mems.filter((m) => LONG_TERM.includes(m.kind)), facts = d.mems.filter((m) => !LONG_TERM.includes(m.kind))
  return (
    <Page title={d.name || d.id} back={back} max={760}
      sub={<Meta><span>{subjectKind(d.type)}</span>{d.mems.length > 0 && <span>{t("knowledge:subject.mems", { count: d.mems.length })}</span>}{d.docs.length > 0 && <span>{t("knowledge:subject.docs", { count: d.docs.length })}</span>}</Meta>}
      actions={d.href ? <Button size="sm" variant="outline" onClick={() => go(d.href)} data-subject-open>{t("knowledge:subject.open", { kind: subjectKind(d.type) })}</Button> : undefined}>
      <div className="flex flex-col gap-8" data-subject-page={d.id}>
        {!d.mems.length && !d.docs.length && <Empty title={t("knowledge:subject.emptyTitle")} sub={t("knowledge:subject.emptySub")} />}
        <Learned mems={d.mems} />
        {long.length > 0 && <Section label={<>{t("knowledge:tab.mems")}<span className="ml-1.5 tabular-nums">{long.length}</span></>} data-subject-section="mems"><Panel>{long.map((m) => <MemRow key={m.id} m={m} />)}</Panel></Section>}
        {facts.length > 0 && <Section label={<>{t("knowledge:tab.entries")}<span className="ml-1.5 tabular-nums">{facts.length}</span></>} data-subject-section="entries"><Panel>{facts.map((m) => <MemRow key={m.id} m={m} />)}</Panel></Section>}
        {d.docs.length > 0 && <Section label={<>{t("knowledge:tab.docs")}<span className="ml-1.5 tabular-nums">{d.docs.length}</span></>} data-subject-section="docs">
          <Panel>{d.docs.map((x) => (
            <Row key={x.id} onClick={() => go(docLink(x))} data-subject-doc={x.id} icon={<Tile I={FileTextIcon} />} title={<span className="truncate">{x.title}</span>}
              sub={<Meta inline><span>{x.source.name}</span>{!!x.used?.count && <span>{t("knowledge:origin.used", { count: x.used.count })}</span>}<span>{t("knowledge:subject.updated", { time: tm(x.updated) })}</span></Meta>}><Chevron /></Row>))}</Panel>
        </Section>}
        {d.uses.length > 0 && <Section label={t("knowledge:subject.uses")} note={t("knowledge:subject.usesNote")} data-subject-section="uses">
          <Panel>{d.uses.map((u) => (
            <Row key={u.consumer_id + u.obj.id} onClick={u.href ? () => go(u.href) : undefined} data-subject-use={u.consumer_type} icon={<Tile I={USE_ICON[u.consumer_type] ?? SearchIcon} />}
              title={<span className="truncate">{u.name}</span>}
              sub={<Meta inline><span className="min-w-0 truncate">{u.obj.label || u.obj.id}</span>{u.query && u.consumer_type !== "search" && <span className="min-w-0 truncate">{t("knowledge:subject.query", { q: u.query })}</span>}<span>{tm(u.created)}</span></Meta>}>{u.href && <Chevron />}</Row>))}</Panel>
        </Section>}
      </div>
    </Page>
  )
}
