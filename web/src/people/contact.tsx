/* #/contacts/:id and #/contacts/me — one contact (prototype people.tsx ContactView) */
import { useLayoutEffect, useRef, useState, type ReactNode } from "react"
import { itemDisplayStatus } from "@/item-workflow-relation"
import { cn } from "cn"
import { CheckIcon, ChevronRightIcon, EllipsisIcon, ListTodoIcon, MailIcon, PencilIcon, MessageSquareIcon, CalendarIcon, PlusIcon, StarIcon, XIcon, ZapIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Field, FieldGroup, FieldLabel } from "@/components/ui/field"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { st, tm } from "@/store"
import { t as tr, useT } from "@/i18n"
import { date } from "@/i18n/format"
import { go, Page, Row, Meta, Empty, Tags, ItemNo, Status, Panel } from "@/ui"
import { AskSteve, type Ctx } from "@/config-ui"
import { nameOf, people, type AccountBrief, type Attr, type Contact, type Level, type Mem, type ProfileField } from "@/api/people"
import { channelsApi } from "@/api/channels"
import { ContactAvatar, LevelSel, Loading, PeopleGate, Section, act, codes, day, joinList, label, relLabel, useLoad, visLabel } from "@/people/common"
import { orgLine } from "@/people/contacts"
import { MergeDialog } from "@/people/pending"
import { HoldingsSection, ResponsibleSection } from "@/people/holdings"
import { AvatarEdit } from "@/people/avatar-edit"
import { Select } from "@/view-shared"

const TOUCH_I: Record<string, any> = { mail: MailIcon, message: MessageSquareIcon, meeting: CalendarIcon, item: ListTodoIcon, event: ZapIcon }
/** the details a page always offers, filled or not: “我” how Steve should address and reach you, others how to reach them */
const ME_KEYS = ["call", "tz", "lang"] as const
const OTHER_KEYS = ["phone", "city"] as const
/** the label of one of those rows, in the UI language */
const attrName = (k: (typeof ME_KEYS)[number] | (typeof OTHER_KEYS)[number]) => tr(`people:contact.attr.${k}`)
/* AskSteve's Ctx names the kinds it knew about; the Hub takes contact / org / team too (contacts.md §5) */
export const ctxOf = (kind: "contact" | "org" | "team" | "global", id?: string) => ({ kind, id }) as unknown as Ctx
const first = (n: string) => n.split(" ")[0]
const keyFor = (label: string) => { let h = 2166136261; for (const ch of label) { h ^= ch.codePointAt(0)!; h = Math.imul(h, 16777619) } return "x_" + (h >>> 0).toString(36) }
/** the memories shown before 显示全部 */
const MEM_FOLD = 5

export function ContactView({ id }: { id: string }) { return <PeopleGate>{() => <ContactPage id={id} />}</PeopleGate> }
function ContactPage({ id }: { id: string }) {
  const t = useT()
  const { data: c, err, reload } = useLoad(() => people.contact(id), [id])
  const back: [string, string] = ["#/contacts", t("people:shared.contacts")]
  if (err) return <Page title={t("people:shared.contacts")} back={back}><Empty title={err.status === 404 ? t("people:contact.notFound") : t("people:contact.loadFailed")} sub={err.status === 404 ? t("people:contact.goneSub") : err.message} /></Page>
  if (!c) return <Page title={t("people:shared.contacts")} back={back}><Loading /></Page>
  if (c.status === "merged" && c.merged_into) return <Page title={c.name} back={back}><Empty title={t("people:contact.merged")} sub={t("people:contact.mergedSub")} /><div className="flex justify-center"><Button variant="outline" onClick={() => go("#/contacts/" + c.merged_into)}>{t("people:contact.seeMerged")}</Button></div></Page>
  return <ContactBody c={c} reload={() => void reload()} />
}

/* The page of a person — “我” or anyone else — in the order a reader wants it: who this is (picture, name, one line of
 * role, the description folded to three lines), what Steve knows about them as one list of label and value where every
 * value is changed in place and carries its disclosure level, the field to tell Steve more, and then only the sections
 * that have something in them (accounts, what they are responsible for, teams, items, contact, memories). */
function ContactBody({ c, reload }: { c: Contact; reload: () => void }) {
  const t = useT()
  const [del, setDel] = useState(false), [edit, setEdit] = useState(false), [merge, setMerge] = useState(false), [job, setJob] = useState(false)
  const items = useLoad(() => people.items(c.id), [c.id], ["party", "item"])
  const tl = useLoad(() => people.timeline(c.id), [c.id])
  const mems = useLoad(() => people.memories(c.id), [c.id])
  const accts = useLoad(() => people.accounts(c.id).catch(() => null), [c.id])
  const orgId = c.org?.id
  const org = useLoad(() => (orgId ? people.org(orgId) : Promise.resolve(null)), [orgId])
  const rels = c.rels.filter((r) => r !== "me"), unconfirmed = c.kind === "unconfirmed"
  const mates = (org.data?.people ?? []).filter((x) => x.id !== c.id)
  const setRels = (r: string) => void act(people.patchContact(c.id, { rels: c.rels.includes(r) ? c.rels.filter((x) => x !== r) : [...c.rels, r] }))
  const role = orgLine(c)
  const sub = c.is_me
    ? <Meta>{role ? (orgId ? <a href={"#/orgs/" + orgId} className="hover:text-foreground">{role}</a> : <span>{role}</span>) : <span>{t("people:contact.isYou")}</span>}</Meta>
    : <Meta>{role && (orgId ? <a href={"#/orgs/" + orgId} className="hover:text-foreground">{role}</a> : <span>{role}</span>)}{rels.length > 0 && <span>{joinList(rels.map(relLabel))}</span>}
      {c.kind && c.kind !== "real" && <span>{label("kind", c.kind)}</span>}{c.aliases.length > 0 && <span>{t("people:shared.aka", { names: joinList(c.aliases) })}</span>}{c.visibility !== "workspace" && <span>{visLabel(c.visibility)}</span>}</Meta>
  return (
    <Page title={nameOf(c)} back={["#/contacts", t("people:shared.contacts")]} max={720} sub={sub}
      lead={<AvatarEdit c={c} size={64} onChanged={reload} />}
      actions={<>
        <Button size="sm" variant="outline" onClick={() => setEdit(true)} data-edit-contact>{t("people:contact.editInfo")}</Button>
        {!c.is_me && <>
          <Button variant="ghost" size="icon-sm" aria-label={c.important ? t("people:contact.unstar") : t("people:contact.star")} aria-pressed={c.important} className="text-muted-foreground" onClick={() => void act(people.patchContact(c.id, { important: !c.important }))} data-star-contact><StarIcon className={cn(c.important && "fill-current text-foreground")} /></Button>
          <DropdownMenu>
            <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label={t("common:action.more")} className="text-muted-foreground" data-contact-more />}><EllipsisIcon /></DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="min-w-44">
              <DropdownMenuItem onClick={() => setMerge(true)} data-merge-contact>{t("people:contact.mergeOther")}</DropdownMenuItem>
              <DropdownMenuSeparator /><DropdownMenuItem variant="destructive" onClick={() => setDel(true)} data-del-contact>{t("common:action.delete")}</DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu></>}</>}>
      <AlertDialog open={del} onOpenChange={setDel}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>{t("people:contact.delTitle", { name: c.name })}</AlertDialogTitle><AlertDialogDescription>{accts.data
          ? t("people:contact.delDescAccounts", { accounts: t("people:contact.accounts", { count: accts.data.length }), attrs: t("people:contact.attrs", { count: c.attrs.length }), mems: t("people:contact.mems", { count: mems.data?.length ?? 0 }) })
          : t("people:contact.delDesc", { attrs: t("people:contact.attrs", { count: c.attrs.length }), mems: t("people:contact.mems", { count: mems.data?.length ?? 0 }) })}</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" data-del-confirm onClick={async () => { if (await act(people.deleteContact(c.id), t("people:contact.deleted"))) go("#/contacts") }}>{t("common:action.delete")}</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
      {edit && <EditDialog c={c} onClose={() => setEdit(false)} onSaved={() => { setEdit(false); reload() }} />}
      {job && <JobDialog c={c} onClose={() => setJob(false)} onSaved={() => { setJob(false); reload() }} />}
      {merge && <MergeDialog from={c} onClose={() => setMerge(false)} />}
      <div className="flex flex-col gap-8">
        {c.kind === "virtual" && <div className="flex flex-wrap items-center justify-between gap-2 rounded-xl bg-muted/60 px-4 py-3 type-callout"><span>{t("people:contact.virtualBanner")}</span><Button size="sm" variant="outline" onClick={async () => { if (await act(people.patchContact(c.id, { kind: "real" }), t("people:contact.restored"))) reload() }}>{t("people:contact.restore")}</Button></div>}
        {unconfirmed && <div className="flex flex-wrap items-center gap-3 rounded-xl bg-card px-5 py-4 shadow-[var(--lift)]" data-unconfirmed-banner>
          <div className="min-w-0 flex-1"><div className="type-headline">{t("people:contact.unconfirmedTitle")}</div><div className="mt-0.5 type-callout text-muted-foreground">{t("people:contact.unconfirmedSub")}</div></div>
          <Button size="sm" variant="outline" onClick={() => setMerge(true)}>{t("people:contact.mergeTo")}</Button>
          <Button size="sm" onClick={() => void act(people.confirm(c.id), t("people:contact.confirmedNew"))} data-confirm-contact>{t("people:contact.confirmNew")}</Button></div>}
        {/* the description, then the tags and (for others) the relation, under the title block */}
        <div className="-mt-4 flex flex-col gap-2">
          {!c.is_me && <Bio text={c.note} empty={t("people:contact.noNote")} onEdit={() => setEdit(true)} />}
          {(c.tags.length > 0 || !c.is_me) && <div className="flex flex-wrap items-center gap-2"><Tags ids={c.tags} />
            {!c.is_me && <DropdownMenu><DropdownMenuTrigger render={<button type="button" className="type-footnote text-muted-foreground hover:text-foreground" data-rel-menu />}>{rels.length ? t("people:contact.changeRel") : t("people:contact.addRel")}</DropdownMenuTrigger>
              <DropdownMenuContent align="start" className="min-w-36">{codes("rel").filter((x) => x !== "me").map((x) => <DropdownMenuItem key={x} onClick={() => setRels(x)} data-rel-opt={x}>{c.rels.includes(x) ? <CheckIcon /> : <span className="size-4" />}{label("rel", x)}</DropdownMenuItem>)}</DropdownMenuContent></DropdownMenu>}</div>}
        </div>
        <Facts c={c} accounts={accts.data} emails={accts.data?.filter((a) => a.system.id === "mail").map((a) => a.handle) ?? null} onJob={() => setJob(true)} reload={() => { reload(); accts.reload() }} />
        <AskSteve key={c.id} ctx={ctxOf("contact", c.id)} placeholder={c.is_me ? t("people:contact.askMe") : t("people:contact.askOther", { name: first(c.name) })} className="-mt-4" />
        {!c.is_me && !!accts.data?.length && <Section label={t("people:contact.linked")} data-linked><Panel>{accts.data.map((a) => <Row key={a.id} title={<span className="break-all">{a.handle}</span>} sub={t("people:contact.accountSub", { system: a.system.name, source: a.source_label })} onClick={() => go("#/accounts/" + a.id)} data-linked-acct={a.handle}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Panel></Section>}
        {c.kind === "bot" ? <HoldingsSection person={c.id} name={first(c.name)} isMe={c.is_me} isBot /> : <ResponsibleSection person={c.id} name={first(c.name)} isMe={c.is_me} />}
        {c.teams.length > 0 && <Section label={t("people:contact.teams", { n: c.teams.length })}><Panel>{c.teams.map((g) => <Row key={g.id} icon={<ContactAvatar name={g.name} type="team" size={28} />} title={g.name} sub={`${label("teamKind", g.kind)} · ${label("role", g.role)}`} onClick={() => go("#/teams/" + g.id)} data-contact-team={g.name}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Panel></Section>}
        {!!items.data?.length && <Section label={t("people:shared.relatedItems", { n: items.data.length })}>
          <Panel>{items.data.map((x) => { const i = st.items.find((y) => y.id === x.item_id); return (
            <Row key={x.item_id + x.role} title={<span><ItemNo id={x.item_id} className="mr-2" />{i?.t ?? x.title ?? ""}</span>} sub={<Meta inline><span>{label("itemRole", x.role)}</span>{x.confidence < 1 && <span>{t("people:shared.inferred")}</span>}</Meta>} onClick={() => go("#/items/" + x.item_id)} data-party-item={x.item_id}>{i && <Status s={itemDisplayStatus(i)} />}</Row>) })}</Panel>
        </Section>}
        {!!tl.data?.length && <Section label={t("people:contact.touches")}>
          <div className="flex flex-col" data-touches>{tl.data.map((x, n) => { const I = TOUCH_I[x.kind] ?? ZapIcon; return (
            <button key={n} type="button" disabled={!x.href} onClick={() => x.href && go(x.href)} className="flex items-start gap-3 rounded-lg px-2 py-2 text-left transition-colors enabled:hover:bg-accent">
              <I className="mt-0.5 size-4 shrink-0 text-muted-foreground" strokeWidth={1.5} aria-hidden /><span className="min-w-0 flex-1 type-callout">{x.text}</span><span className="shrink-0 type-footnote tabular-nums text-muted-foreground">{label("touch", x.kind)} · {tm(x.ts)}</span></button>) })}</div>
        </Section>}
        {c.is_me && !!c.note.trim() && <details className="rounded-xl bg-card px-4 py-3 shadow-[var(--lift)]" data-original-note>
          <summary className="cursor-pointer type-callout text-muted-foreground">{t("people:me.originalNote")}</summary>
          <p className="mt-3 whitespace-pre-wrap break-words type-body">{c.note}</p>
          <p className="mt-2 type-footnote text-muted-foreground">{t("people:me.noteHint")}</p>
          <Button size="xs" variant="ghost" onClick={() => setEdit(true)}>{t("people:contact.editInfo")}</Button>
        </details>}
        <MemSection party={c.id} mems={mems.data} label={c.is_me ? t("people:contact.notedMe") : t("people:contact.notedOther", { name: first(c.name) })} hideEmpty fold={MEM_FOLD} />
        {c.merges.length > 0 && <Section label={t("people:contact.merges")}><Panel>{c.merges.map((m) => (
          <Row key={m.id} title={t("people:contact.mergedFrom", { name: m.from_name })} sub={m.splittable ? t("people:contact.mergeSplittable", { day: day(m.created) }) : day(m.created)} data-merge-rec={m.id}>
            {m.splittable && <Button size="xs" variant="outline" onClick={() => void act(people.split(m.id), t("people:contact.splitDone"))} data-split={m.id}>{t("people:contact.split")}</Button>}</Row>))}</Panel></Section>}
        {mates.length > 0 && <Section label={t("people:contact.mates")}><Panel>{mates.map((x) => <Row key={x.id} icon={<ContactAvatar name={x.name} size={28} />} title={x.name} sub={x.title ?? undefined} onClick={() => go("#/contacts/" + x.id)}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Panel></Section>}
      </div>
    </Page>
  )
}

/** the description, folded to three lines with 展开 when it is longer; none yet is a quiet way to write one */
function Bio({ text, empty, onEdit }: { text: string; empty: string; onEdit: () => void }) {
  const t = useT()
  const ref = useRef<HTMLDivElement>(null)
  const [open, setOpen] = useState(false), [over, setOver] = useState(false)
  useLayoutEffect(() => {
    const el = ref.current
    if (!el) return
    const measure = () => setOver(el.scrollHeight > el.clientHeight + 1)
    measure()
    const ro = new ResizeObserver(measure); ro.observe(el)
    return () => ro.disconnect()
  }, [text, open])
  if (!text.trim()) return <button type="button" onClick={onEdit} className="-mx-1.5 self-start rounded-md px-1.5 py-0.5 text-left type-body text-muted-foreground transition-colors duration-120 hover:bg-accent hover:text-foreground" data-bio-empty>{empty}</button>
  return (
    <div className="flex flex-col items-start gap-1">
      <div ref={ref} className={cn("type-body whitespace-pre-wrap text-foreground/85", !open && "line-clamp-3")} data-bio>{text}</div>
      {(over || open) && <button type="button" onClick={() => setOpen(!open)} aria-expanded={open} className="type-callout text-primary hover:underline" data-bio-toggle>{open ? t("people:contact.bioLess") : t("people:contact.bioMore")}</button>}
    </div>
  )
}

/** “Steve 记得的你”: one list of label and value. Every value changes in place (a job opens its dialog: it needs an
 *  organization), an empty one says 补充, and a detail carries its disclosure level beside it. Why an empty field could
 *  not be filled in automatically is said once under the list, not on every row. */
function Facts({ c, accounts, emails, onJob, reload }: { c: Contact; accounts: AccountBrief[] | null; emails: string[] | null; onJob: () => void; reload: () => void }) {
  const t = useT()
  const [adding, setAdding] = useState(false)
  if (c.is_me) return <MeFacts c={c} accounts={accounts} onJob={onJob} reload={reload} />
  const fixed: readonly string[] = c.is_me ? ME_KEYS : OTHER_KEYS
  const now = c.jobs.filter((j) => !j.to), past = c.jobs.filter((j) => j.to)
  const orgs = joinList(now.map((j) => j.org_name)), titles = joinList(now.map((j) => [j.dept, j.title].filter(Boolean).join(" · ")).filter(Boolean))
  const keyed = (k: string) => {
    const xs = c.attrs.filter((a) => a.key === k)
    return xs.length ? xs.map((a) => <AttrFact key={a.id} a={a} onChanged={reload} />)
      : [<Fact key={k} label={attrName(k as (typeof ME_KEYS)[number])} data-fact={k}>
          <Value label={attrName(k as (typeof ME_KEYS)[number])} onSave={async (v) => { const r = await act(people.addAttr(c.id, { key: k, label: attrName(k as (typeof ME_KEYS)[number]), value: v })); if (r) reload(); return !!r }} /></Fact>]
  }
  const job = [
    <Fact key="org" label={t("people:contact.field.org")} sub={past.length ? t("people:contact.before", { jobs: past.map((p) => t("people:contact.pastJob", { job: `${p.org_name}${p.title ? " · " + p.title : ""}`, to: p.to })).join(t("people:shared.clauses")) }) : undefined} data-fact="org">
      <Value label={t("people:contact.field.org")} value={orgs} onOpen={onJob} /></Fact>,
    <Fact key="title" label={t("people:contact.field.title")} data-fact="title"><Value label={t("people:contact.field.title")} value={titles} onOpen={onJob} /></Fact>,
    emails && <Fact key="email" label={t("people:contact.field.email")} data-fact="email">
      {emails.length ? <span className="break-all type-body">{joinList(emails)}</span>
        : <Value label={t("people:contact.field.email")} type="email" onSave={async (v) => { const r = await act(people.addAccount({ system_id: "mail", party_id: c.id, kind: "personal", handle: v }), t("people:email.added")); if (r) reload(); return !!r }} />}</Fact>,
  ]
  const rest = c.attrs.filter((a) => !fixed.includes(a.key)).map((a) => <AttrFact key={a.id} a={a} onChanged={reload} />)
  /* why the empty fields stay empty, once: the uninformative “no account to look it up with” is left out */
  const why = new Map<string, string[]>()
  for (const [k, f] of Object.entries(c.profile?.fields ?? {})) if (f.state === "missing" && f.reason && f.reason !== "no_source" && k !== "name") { const r = profileReason(f); why.set(r, [...(why.get(r) ?? []), t(`people:contact.field.${k as "org"}`)]) }
  const pending = Object.values(c.profile?.fields ?? {}).some((f) => f.state === "missing" && f.reason === "pending")
  const notes = [...why].map(([r, ks]) => t("people:contact.why", { fields: joinList(ks), reason: r }))
  if (pending && c.profile?.next_check) notes.push(t("people:contact.nextCheck", { time: date(c.profile.next_check, { year: "numeric", month: "numeric", day: "numeric", hour: "numeric", minute: "2-digit" }) }))
  return (
    <Section label={c.is_me ? t("people:contact.knownMe") : t("people:contact.knownOther", { name: first(c.name) })} data-facts
      note={(notes.length > 0 || c.attrs.length > 0) && <>{notes.map((n) => <p key={n} data-fact-why>{n}</p>)}{c.attrs.length > 0 && <p>{t("people:contact.levelHint")}</p>}</>}>
      <Panel>
        {c.is_me ? [...fixed.flatMap(keyed), ...job] : [...job, ...fixed.flatMap(keyed)]}
        {rest}
        {adding ? <NewAttr party={c.id} init={{ label: "" }} onDone={() => { setAdding(false); reload() }} />
          : <button type="button" onClick={() => setAdding(true)} className="flex min-h-11 w-full items-center gap-1.5 rounded-lg px-3 text-left type-callout text-muted-foreground transition-colors duration-120 hover:bg-accent hover:text-foreground" data-add-attr><PlusIcon className="size-4" strokeWidth={1.75} />{t("people:contact.addFact")}</button>}
      </Panel>
    </Section>
  )
}

/** Structured facts, not a heuristic parser of a person's prose. Unknown custom facts stay visible.
 * Accounts/jobs are read from their owning modules; the Hub never probes the browser host or git. */
function MeFacts({ c, accounts, onJob, reload }: { c: Contact; accounts: AccountBrief[] | null; onJob: () => void; reload: () => void }) {
  const t = useT()
  const [adding, setAdding] = useState<string | null>(null)
  const [scope, setScope] = useState("persona"), [scopeId, setScopeId] = useState(""), [scopeName, setScopeName] = useState("")
  const targets = useLoad(async () => {
    const [personas, spaces] = await Promise.all([channelsApi.personas(), channelsApi.spaces()])
    return { persona: personas.map(p => ({ v: p.id, l: p.name })), space: spaces.filter(s => s.status === "active").map(s => ({ v: s.id, l: s.title || s.name })) }
  }, [])
  const options = targets.data?.[scope as "persona" | "space"] ?? []
  const groups = [
    { id: "identity", keys: ["role"] },
    { id: "work", keys: ["project", "repository", "tools"] },
    { id: "contact", keys: ["email", "phone", "website"] },
    { id: "preferences", keys: ["lang", "tz", "communication"] },
  ]
  const allKeys = groups.flatMap(g => g.keys).concat(["call", "call_external", "call_ext_scope"])
  const attrs = c.attrs.filter(a => a.status !== "stale")
  const matchesAccount = (a: Attr, account: AccountBrief) => a.key === "email" && a.status === "confirmed" && account.system.id === "mail" && a.value.trim().toLowerCase() === account.handle.trim().toLowerCase()
  const field = (key: string) => {
    const name = t(`people:me.fields.${key as "role"}`), found = attrs.filter(a => a.key === key)
    return found.length ? found.map(a => <AttrFact key={a.id} a={{ ...a, label: name }} accounts={accounts?.filter(account => matchesAccount(a, account))} onChanged={reload} />)
      : <Fact key={key} label={name} data-fact={key}><Value label={name} onSave={async value => {
          const ok = await act(people.addAttr(c.id, { key, label: name, value })); if (ok) reload(); return !!ok
        }} /></Fact>
  }
  const source = (value: string) => <Badge size="sm" variant="secondary">{value}</Badge>
  const jobs = c.jobs.filter(j => !j.to)
  return <div className="flex flex-col gap-6" data-me-facts>
    <Section label={t("people:me.address")} note={t("people:me.addressHint")} data-address-facts><Panel>
      {field("call")}{field("call_external")}
      {attrs.filter(a => a.key === "call_ext_scope").map(a => {
        let scopeLabel: string | undefined
        try {
          const v = JSON.parse(a.value)
          scopeLabel = targets.data?.[v.kind as "persona" | "space"]?.find(o => o.v === v.id)?.l
        } catch { /* historical malformed facts remain readable */ }
        return <AttrFact key={a.id} a={a} scopeLabel={scopeLabel} onChanged={reload} />
      })}
      <details className="px-3 py-2" data-address-overrides><summary className="cursor-pointer type-callout text-muted-foreground">{t("people:me.overrides")}</summary>
        <div className="mt-3 flex flex-wrap gap-2">
          <Select label={t("people:me.scope")} value={scope} onChange={value => { setScope(value); setScopeId("") }} options={[{ v: "persona", l: t("people:me.persona") }, { v: "space", l: t("people:me.space") }]} />
          <Select className="min-w-40 flex-1" label={t("people:me.scopeId")} placeholder={t("people:me.scopeId")} value={scopeId} onChange={setScopeId} options={options} disabled={!options.length} />
          <Input className="min-w-40 flex-1" aria-label={t("people:me.externalName")} placeholder={t("people:me.externalName")} maxLength={80} value={scopeName} onChange={e => setScopeName(e.target.value)} />
          <Button size="sm" disabled={!scopeId.trim() || !scopeName.trim()} onClick={async () => {
            if (await act(people.addAttr(c.id, { key: "call_ext_scope", label: `${scope}:${keyFor(scopeId.trim())}`, value: JSON.stringify({ kind: scope, id: scopeId.trim(), name: scopeName.trim() }), level: "public" }))) { setScopeId(""); setScopeName(""); reload() }
          }}>{t("common:action.save")}</Button>
        </div>
        {targets.err && <p role="alert" className="mt-2 type-footnote text-destructive">{targets.err.message}</p>}
        {targets.data && !options.length && <p className="mt-2 type-footnote text-muted-foreground">{t("people:me.noScopes")}</p>}
      </details>
    </Panel></Section>
    {groups.map(g => <Section key={g.id} label={t(`people:me.groups.${g.id as "identity"}`)} data-fact-group={g.id}><Panel>
      {g.id === "identity" && <>
        <Fact label={t("people:contact.field.org")} extra={jobs.length ? source(jobs.some(j => j.source !== "manual") ? t("people:me.accountProfile") : t("people:source.manual")) : undefined}><Value label={t("people:contact.field.org")} value={joinList(jobs.map(j => j.org_name))} onOpen={onJob} /></Fact>
        <Fact label={t("people:contact.field.title")} extra={jobs.length ? source(jobs.some(j => j.source !== "manual") ? t("people:me.accountProfile") : t("people:source.manual")) : undefined}><Value label={t("people:contact.field.title")} value={joinList(jobs.map(j => j.title ?? "").filter(Boolean))} onOpen={onJob} /></Fact>
      </>}
      {g.keys.flatMap(field)}
      {g.id === "contact" && accounts?.filter(account => !attrs.some(a => matchesAccount(a, account))).map(a => <Fact key={a.id} label={a.system.name} extra={source(t("people:me.accountProfile"))} data-platform-account={a.id}><a className="break-all type-body hover:underline" href={"#/accounts/" + a.id}>{a.handle}</a></Fact>)}
      {adding === g.id ? <NewAttr party={c.id} init={{ label: "" }} onDone={() => { setAdding(null); reload() }} /> : null}
    </Panel></Section>)}
    {attrs.some(a => !allKeys.includes(a.key)) && <Section label={t("people:me.otherFacts")}><Panel>{attrs.filter(a => !allKeys.includes(a.key)).map(a => <AttrFact key={a.id} a={a} onChanged={reload} />)}</Panel></Section>}
    <Button variant="ghost" className="self-start text-muted-foreground" onClick={() => setAdding("preferences")} data-add-attr><PlusIcon />{t("people:contact.addFact")}</Button>
    <p className="-mt-3 type-footnote text-muted-foreground">{t("people:me.sourcesHint")}</p>
  </div>
}

/** one line of the list: the label, the value, and its level and removal on the right. On a phone a line with a level
 *  puts the value under the label, so the level never squeezes it; a line without one stays a single line */
function Fact({ label: name, extra, sub, level, onDelete, deleteLabel, children, ...rest }: { label: ReactNode; extra?: ReactNode; sub?: ReactNode; level?: ReactNode; onDelete?: () => void; deleteLabel?: string; children: ReactNode; [k: `data-${string}`]: unknown }) {
  const side = !!(level || onDelete)
  return (
    <div role="listitem" className={cn("group/fact grid min-h-11 items-start gap-x-3 px-3 py-2 sm:grid-cols-[9rem_minmax(0,1fr)_auto]", side ? "grid-cols-[minmax(0,1fr)_auto]" : "grid-cols-[5.5rem_minmax(0,1fr)]")} {...rest}>
      <div className="flex min-h-7 min-w-0 flex-wrap items-center gap-1.5 type-callout text-muted-foreground">{name}{extra}</div>
      <div className={cn("flex min-h-7 min-w-0 flex-col justify-center", side && "max-sm:col-span-2 max-sm:row-start-2")}>{children}{sub && <div className="mt-0.5 type-footnote text-muted-foreground">{sub}</div>}</div>
      {side && <div className="flex min-h-7 items-center justify-end gap-0.5 max-sm:col-start-2 max-sm:row-start-1">{level}
        {onDelete && <Button variant="ghost" size="icon-xs" aria-label={deleteLabel} className="text-muted-foreground transition-opacity duration-120 [@media(hover:hover)]:opacity-0 [@media(hover:hover)]:group-hover/fact:opacity-100 [@media(hover:hover)]:group-focus-within/fact:opacity-100" onClick={onDelete} data-del-attr={deleteLabel}><XIcon /></Button>}</div>}
    </div>
  )
}

/** a value that changes in place: click it (or 补充) for a field; Enter or leaving it saves, Esc puts it back.
 *  `onOpen` instead: the value is changed somewhere else (a dialog) */
function Value({ label: name, value, onSave, onOpen, type }: { label: string; value?: string; onSave?: (v: string) => Promise<boolean>; onOpen?: () => void; type?: string }) {
  const t = useT()
  const [editing, setEditing] = useState(false), [v, setV] = useState(value ?? ""), [busy, setBusy] = useState(false)
  const done = useRef(false)
  const open = () => { if (onOpen) return onOpen(); setV(value ?? ""); done.current = false; setEditing(true) }
  const commit = async () => {
    if (done.current) return
    const x = v.trim()
    if (!x || x === (value ?? "")) { done.current = true; setEditing(false); return }
    done.current = true; setBusy(true)
    const ok = await onSave!(x)
    setBusy(false)
    if (ok) setEditing(false); else done.current = false
  }
  if (editing) return <Input autoFocus type={type} className="h-8 w-full max-w-sm" aria-label={t("people:attr.editValue", { label: name })} value={v} maxLength={500} disabled={busy}
    onChange={(e) => setV(e.target.value)} onBlur={() => void commit()}
    onKeyDown={(e) => { if (e.key === "Enter" && !e.nativeEvent.isComposing) void commit(); if (e.key === "Escape") { done.current = true; setEditing(false) } }} data-fact-input />
  if (!value) return <button type="button" onClick={open} className="-mx-1.5 inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 type-callout text-tertiary transition-colors duration-120 hover:bg-accent hover:text-foreground" aria-label={t("people:contact.fillLabel", { label: name })} data-fill={name}>
    <PlusIcon className="size-3.5" strokeWidth={1.75} />{t("people:contact.fill")}</button>
  return <button type="button" onClick={open} className="group/v -mx-1.5 inline-flex max-w-full items-start gap-1.5 rounded-md px-1.5 py-0.5 text-left type-body transition-colors duration-120 hover:bg-accent" aria-label={t("people:attr.edit", { label: name })} data-value={name}>
    <span className="min-w-0 break-words">{value}</span><PencilIcon className="mt-1 size-3.5 shrink-0 text-tertiary opacity-0 transition-opacity duration-120 group-hover/v:opacity-100 group-focus-visible/v:opacity-100" strokeWidth={1.75} /></button>
}

/** a detail of the contact in the list: its value, its level, and removing it */
function AttrFact({ a, scopeLabel, accounts, onChanged }: { a: Attr; scopeLabel?: string; accounts?: AccountBrief[]; onChanged: () => void }) {
  const t = useT()
  let scoped: { kind?: string; id?: string; name?: string } | null = null
  if (a.key === "call_ext_scope") { try { scoped = JSON.parse(a.value) } catch { /* malformed historical facts stay readable */ } }
  const shown = scoped?.name ?? a.value
  return (
    <Fact label={scoped ? t("people:me.fields.call_external") : a.label} sub={scoped?.id ? `${scoped.kind === "space" ? t("people:me.space") : t("people:me.persona")} · ${scopeLabel ?? scoped.id}` : accounts?.length ? <span className="flex flex-wrap gap-2">{accounts.map(account => <a key={account.id} className="hover:underline" href={"#/accounts/" + account.id} data-platform-account={account.id}>{t("people:contact.linked")} · {account.system.name}</a>)}</span> : undefined} data-attr={a.label} onDelete={async () => { if (await act(people.deleteAttr(a.id))) onChanged() }} deleteLabel={t("people:attr.del", { label: a.label })}
      extra={<><Badge variant="secondary" size="sm" data-fact-source>{a.status === "candidate" ? t("people:shared.inferred") : a.source === "profile" ? t("people:me.accountProfile") : a.source_label}</Badge>{a.status === "candidate" && <Badge variant="secondary" className="bg-attention/12 text-attention">{t("people:shared.pending")}</Badge>}{a.status === "stale" && <Badge variant="secondary" className="text-muted-foreground">{t("people:shared.stale")}</Badge>}</>}
      level={<LevelSel v={a.level} on={(l) => void act(people.patchAttr(a.id, { level: l }))} testId={"level-" + a.key} />}>
      {a.status === "candidate" && <div className="mb-1 flex flex-wrap items-center gap-1">
        <span className="type-footnote text-attention">{t("people:shared.inferred")}</span>
        <Button size="xs" variant="outline" onClick={async () => { if (await act(people.patchAttr(a.id, { status: "confirmed" }))) onChanged() }} data-fact-confirm>{t("common:action.confirm")}</Button>
        <Button size="xs" variant="ghost" onClick={async () => { if (await act(people.patchAttr(a.id, { status: "stale" }))) onChanged() }} data-fact-reject>{t("people:shared.wrong")}</Button>
      </div>}
      <Value label={a.label} value={shown} onSave={async (v) => { const r = await act(people.patchAttr(a.id, { value: scoped ? JSON.stringify({ ...scoped, name: v }) : v })); if (r) onChanged(); return !!r }} />
    </Fact>
  )
}

/** a row whose right side is a value and its controls: on a phone they take their own line under the label instead of
 *  squeezing it to a character per line */
const STACKED = "max-sm:flex-wrap max-sm:[&>[data-slot=item-content]]:basis-full max-sm:[&>[data-slot=item-actions]]:w-full max-sm:[&>[data-slot=item-actions]]:shrink"
export function AttrRow({ a, readOnly, onChanged }: { a: Attr; readOnly?: boolean; onChanged?: () => void }) {
  const t = useT()
  const [editing, setEditing] = useState(false), [value, setValue] = useState(a.value), [busy, setBusy] = useState(false)
  const save = async () => { setBusy(true); const r = await act(people.patchAttr(a.id, { value: value.trim() })); setBusy(false); if (r) { setEditing(false); onChanged?.() } }
  return (
    <Row title={<span className="inline-flex items-center gap-2">{a.label}{a.status === "candidate" && <Badge variant="secondary" className="bg-attention/12 text-attention">{t("people:shared.pending")}</Badge>}{a.status === "stale" && <Badge variant="secondary" className="text-muted-foreground">{t("people:shared.stale")}</Badge>}</span>} sub={t("people:shared.source", { source: a.source_label })} className={STACKED} data-attr={a.label}>
      {editing ? <div className="flex min-w-0 flex-wrap gap-1"><Input aria-label={t("people:attr.editValue", { label: a.label })} value={value} maxLength={500} onChange={(e) => setValue(e.target.value)} /><Button size="xs" disabled={busy || !value.trim()} onClick={() => void save()}>{t("common:action.save")}</Button><Button size="xs" variant="ghost" disabled={busy} onClick={() => setEditing(false)}>{t("common:action.cancel")}</Button></div> : <span className="min-w-0 break-words type-callout max-sm:flex-1 sm:max-w-[260px]">{a.value}</span>}
      {readOnly ? <span className="type-footnote text-muted-foreground">{label("level", a.level)}</span> : <>
        <Button size="icon-xs" variant="ghost" aria-label={t("people:attr.edit", { label: a.label })} disabled={busy} onClick={() => { setValue(a.value); setEditing(true) }}><PencilIcon /></Button>
        <LevelSel v={a.level} on={(l) => void act(people.patchAttr(a.id, { level: l }))} testId={"level-" + a.key} />
        <Button variant="ghost" size="icon-xs" aria-label={t("people:attr.del", { label: a.label })} className="text-muted-foreground" onClick={() => void act(people.deleteAttr(a.id))} data-del-attr={a.label}><XIcon /></Button></>}
    </Row>
  )
}

export function NewAttr({ party, init, onDone }: { party: string; init: { key?: string; label: string }; onDone: () => void }) {
  const t = useT()
  const [label, setLabel] = useState(init.label), [v, setV] = useState(""), [busy, setBusy] = useState(false)
  const defs = useLoad(() => people.attrDefs().catch(() => []), [])
  const save = async () => {
    const l = label.trim(), d = defs.data?.find((x) => x.label === l || x.key === l)
    setBusy(true)
    const r = await act(people.addAttr(party, { key: init.key ?? d?.key ?? keyFor(l), label: l.slice(0, 20), value: v.trim() }))
    setBusy(false); if (r) onDone()
  }
  return (
    <div className="flex flex-wrap items-center gap-2 px-2 py-2.5" role="group" aria-label={t("people:attr.add")}>
      <Input className="h-8 w-32 rounded-lg" aria-label={t("people:attr.nameLabel")} placeholder={t("people:attr.namePh")} list="attr-defs" value={label} disabled={!!init.key} onChange={(e) => setLabel(e.target.value)} data-attr-k />
      <datalist id="attr-defs">{(defs.data ?? []).map((d) => <option key={d.id} value={d.label} />)}</datalist>
      <Input autoFocus className="h-8 min-w-40 flex-1 rounded-lg" aria-label={t("people:attr.value")} placeholder={t("people:attr.value")} maxLength={500} value={v} onChange={(e) => setV(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && label.trim() && v.trim() && !e.nativeEvent.isComposing) void save() }} data-attr-v />
      <Button size="sm" disabled={!label.trim() || !v.trim() || busy} onClick={() => void save()} data-attr-save>{t("common:action.save")}</Button><Button size="sm" variant="ghost" onClick={onDone}>{t("common:action.cancel")}</Button>
    </div>
  )
}

export function MemSection({ party, mems, label: heading, editable = true, hideEmpty = false, fold }: { party: string; mems: Mem[] | null; label: string; editable?: boolean; hideEmpty?: boolean; fold?: number }) {
  const t = useT()
  const [adding, setAdding] = useState(false), [text, setText] = useState(""), [all, setAll] = useState(false)
  const add = async () => { if (await act(people.addMemory(party, { text: text.trim() }))) { setText(""); setAdding(false) } }
  if (hideEmpty && !adding && (!mems || !mems.length)) return null
  const shown = mems && fold && !all ? mems.slice(0, fold) : mems
  return (
    <Section label={`${heading} · ${mems?.length ?? 0}`} right={editable ? <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setAdding(true)} data-add-mem><PlusIcon />{t("common:action.add")}</Button> : undefined} data-mems>
      {adding && <div className="mb-2 flex items-center gap-2"><Input autoFocus className="h-8 flex-1 rounded-lg" aria-label={t("people:mem.newLabel")} placeholder={t("people:mem.newPh")} maxLength={2000} value={text} onChange={(e) => setText(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && text.trim() && !e.nativeEvent.isComposing) void add() }} data-mem-input />
        <Button size="sm" disabled={!text.trim()} onClick={() => void add()} data-mem-save>{t("people:mem.keep")}</Button><Button size="sm" variant="ghost" onClick={() => setAdding(false)}>{t("common:action.cancel")}</Button></div>}
      {!mems || !shown ? <Loading /> : mems.length ? <Panel>{shown.map((m) => (
        <Row key={m.id} title={<span className="inline-flex items-center gap-2">{m.text}{m.status === "candidate" && <Badge variant="secondary" className="bg-attention/12 text-attention">{t("people:shared.pending")}</Badge>}{m.status === "stale" && <Badge variant="secondary" className="text-muted-foreground">{t("people:shared.stale")}</Badge>}</span>} sub={t("people:mem.sub", { kind: label("memKind", m.kind), source: m.source_label })} className={STACKED} data-memo={m.text}>
          {m.status === "candidate" && <><Button size="xs" variant="outline" onClick={() => void act(people.patchMem(m.id, { status: "confirmed" }))} data-memo-ok>{t("common:action.confirm")}</Button><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => void act(people.deleteMem(m.id))}>{t("people:shared.wrong")}</Button></>}
          <LevelSel v={m.level} on={(l: Level) => void act(people.patchMem(m.id, { level: l }))} />
          {m.status !== "candidate" && <Button variant="ghost" size="icon-xs" aria-label={t("people:mem.del")} className="text-muted-foreground" onClick={() => void act(people.deleteMem(m.id))}><XIcon /></Button>}</Row>))}</Panel>
        : <Empty title={t("people:mem.empty")} sub={t("people:mem.emptySub")} />}
      {mems && fold && mems.length > fold && <button type="button" onClick={() => setAll(!all)} aria-expanded={all} className="mt-1 px-1 type-callout text-primary hover:underline" data-mems-toggle>{all ? t("people:contact.bioLess") : t("people:contact.memsMore", { count: mems.length - fold })}</button>}
    </Section>
  )
}

function EditDialog({ c, onClose, onSaved }: { c: Contact; onClose: () => void; onSaved: () => void }) {
  const t = useT()
  const [n, setN] = useState(c.name), [al, setAl] = useState(joinList(c.aliases)), [note, setNote] = useState(c.note), [busy, setBusy] = useState(false)
  const save = async () => {
    setBusy(true)
    const aliases = al.split(/[、,，\s]+/).map((x) => x.trim()).filter(Boolean).slice(0, 10)
    const r = await act(people.patchContact(c.id, { name: n.trim(), aliases, note }))
    setBusy(false); if (r) onSaved()
  }
  return (
    <Dialog open onOpenChange={(x) => !x && onClose()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[460px]">
        <DialogHeader><DialogTitle className="type-headline">{t("people:editContact.title", { name: c.name })}</DialogTitle><DialogDescription className="type-callout">{t("people:editContact.desc")}</DialogDescription></DialogHeader>
        <FieldGroup className="gap-4">
          <Field><FieldLabel htmlFor="ec-name">{t("people:shared.name")}</FieldLabel><Input id="ec-name" className="rounded-lg" maxLength={80} value={n} onChange={(e) => setN(e.target.value)} data-ec-name /></Field>
          <Field><FieldLabel htmlFor="ec-alias">{t("people:editContact.aliases")}</FieldLabel><Input id="ec-alias" className="rounded-lg" value={al} onChange={(e) => setAl(e.target.value)} placeholder={t("people:editContact.aliasesPh")} data-ec-alias /></Field>
          <Field><FieldLabel htmlFor="ec-note">{t("people:shared.note")}</FieldLabel><Textarea id="ec-note" className="min-h-20 rounded-lg" maxLength={2000} value={note} onChange={(e) => setNote(e.target.value)} data-ec-note /></Field>
        </FieldGroup>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button><Button disabled={!n.trim() || busy} onClick={() => void save()} data-ec-save>{t("common:action.save")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function JobDialog({ c, onClose, onSaved }: { c: Contact; onClose: () => void; onSaved: () => void }) {
  const t = useT()
  const orgs = useLoad(() => people.orgs(), [])
  const [orgId, setOrgId] = useState(c.jobs.find((j) => !j.to)?.org_id ?? ""), [title, setTitle] = useState(c.jobs.find((j) => !j.to)?.title ?? ""), [dept, setDept] = useState(c.jobs.find((j) => !j.to)?.dept ?? ""), [busy, setBusy] = useState(false)
  const choose = (id: string) => { setOrgId(id); const j = c.jobs.find((x) => x.org_id === id); setTitle(j?.title ?? ""); setDept(j?.dept ?? "") }
  const save = async () => {
    setBusy(true)
    const r = await act(c.jobs.some((j) => j.org_id === orgId) ? people.patchJob(c.id, orgId, { title: title.trim() || null, dept: dept.trim() || null, to: null }) : people.addJob(c.id, { org_id: orgId, title: title.trim(), dept: dept.trim() }), t("people:job.saved"))
    setBusy(false); if (r) onSaved()
  }
  return <Dialog open onOpenChange={(v) => !v && !busy && onClose()}><DialogContent>
    <DialogHeader><DialogTitle>{t("people:job.title")}</DialogTitle><DialogDescription>{t("people:job.desc")}</DialogDescription></DialogHeader>
    <div className="flex flex-col gap-1.5 type-callout font-medium">{t("people:shared.org")}<Select size="default" label={t("people:job.orgAria")} className="w-full font-normal" value={orgId} onChange={choose} placeholder={t("people:job.pickOrg")} options={(orgs.data ?? []).map((o) => ({ v: o.id, l: o.name }))} /></div>
    {orgs.err && <p role="alert" className="type-callout text-destructive">{t("people:job.orgFailed", { msg: orgs.err.message })}</p>}
    <label className="flex flex-col gap-1.5 type-callout font-medium">{t("people:job.dept")}<Input aria-label={t("people:job.dept")} value={dept} onChange={(e) => setDept(e.target.value)} maxLength={80} /></label>
    <label className="flex flex-col gap-1.5 type-callout font-medium">{t("people:job.position")}<Input aria-label={t("people:job.position")} value={title} onChange={(e) => setTitle(e.target.value)} maxLength={80} /></label>
    <DialogFooter><Button variant="outline" disabled={busy} onClick={onClose}>{t("common:action.cancel")}</Button><Button disabled={busy || !orgId} onClick={() => void save()}>{t("common:action.save")}</Button></DialogFooter>
  </DialogContent></Dialog>
}

function profileReason(field: ProfileField) {
  const reason = field.reason ? tr(`people:profile.${field.reason}`) : tr("people:profile.none")
  return field.scope ? tr("people:profile.withScope", { reason, scope: field.scope }) : reason
}
