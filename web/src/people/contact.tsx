/* #/contacts/:id and #/contacts/me — one contact (prototype people.tsx ContactView) */
import { useState } from "react"
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
import { go, Page, Group, Row, Meta, Empty, Tags, ItemNo, Status } from "@/ui"
import { AskSteve, type Ctx } from "@/config-ui"
import { nameOf, people, type Attr, type Contact, type Level, type Mem, type ProfileField } from "@/api/people"
import { ContactAvatar, LevelSel, Loading, PeopleGate, Section, act, codes, day, joinList, label, relLabel, useLoad, visLabel } from "@/people/common"
import { orgLine } from "@/people/contacts"
import { MergeDialog } from "@/people/pending"
import { HoldingsSection } from "@/people/holdings"
import { Select } from "@/view-shared"

const TOUCH_I: Record<string, any> = { mail: MailIcon, message: MessageSquareIcon, meeting: CalendarIcon, item: ListTodoIcon, event: ZapIcon }
/** “我”的页先列这几项，没填的也占一行 */
const ME_KEYS = ["call", "tz", "lang"] as const
const OTHER_KEYS = ["phone", "city"] as const
/** the label of one of those rows, in the UI language */
const attrName = (k: (typeof ME_KEYS)[number] | (typeof OTHER_KEYS)[number]) => tr(`people:contact.attr.${k}`)
/* AskSteve's Ctx names the kinds it knew about; the Hub takes contact / org / team too (contacts.md §5) */
export const ctxOf = (kind: "contact" | "org" | "team" | "global", id?: string) => ({ kind, id }) as unknown as Ctx
const first = (n: string) => n.split(" ")[0]
const keyFor = (label: string) => { let h = 2166136261; for (const ch of label) { h ^= ch.codePointAt(0)!; h = Math.imul(h, 16777619) } return "x_" + (h >>> 0).toString(36) }

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

function ContactBody({ c, reload }: { c: Contact; reload: () => void }) {
  const t = useT()
  const [del, setDel] = useState(false), [edit, setEdit] = useState(false), [merge, setMerge] = useState(false), [job, setJob] = useState(false), [email, setEmail] = useState(false)
  const [newAttr, setNewAttr] = useState<{ key?: string; label: string } | null>(null)
  const items = useLoad(() => people.items(c.id), [c.id], ["party", "item"])
  const tl = useLoad(() => people.timeline(c.id), [c.id])
  const mems = useLoad(() => people.memories(c.id), [c.id])
  const accts = useLoad(() => people.accounts(c.id).catch(() => null), [c.id])
  const orgId = c.org?.id
  const org = useLoad(() => (orgId ? people.org(orgId) : Promise.resolve(null)), [orgId])
  const rels = c.rels.filter((r) => r !== "me"), unconfirmed = c.kind === "unconfirmed"
  const mates = (org.data?.people ?? []).filter((x) => x.id !== c.id)
  const past = c.jobs.filter((j) => j.to), now = c.jobs.filter((j) => !j.to)
  const attrs = c.is_me ? [...ME_KEYS.flatMap((k) => c.attrs.filter((a) => a.key === k)), ...c.attrs.filter((a) => !ME_KEYS.some((k) => k === a.key))] : c.attrs
  const missing = (c.is_me ? ME_KEYS : OTHER_KEYS).filter((k) => !c.attrs.some((a) => a.key === k)).map((k) => [k, attrName(k)] as const)
  const setRels = (r: string) => void act(people.patchContact(c.id, { rels: c.rels.includes(r) ? c.rels.filter((x) => x !== r) : [...c.rels, r] }))
  return (
    <Page title={nameOf(c)} back={["#/contacts", t("people:shared.contacts")]} max={760}
      sub={<Meta>{orgLine(c) && (orgId ? <a href={"#/orgs/" + orgId} className="hover:text-foreground">{orgLine(c)}</a> : <span>{orgLine(c)}</span>)}{rels.length > 0 && <span>{joinList(rels.map(relLabel))}</span>}
        {c.kind && c.kind !== "real" && <span>{label("kind", c.kind)}</span>}{c.aliases.length > 0 && <span>{t("people:shared.aka", { names: joinList(c.aliases) })}</span>}<span>{visLabel(c.visibility)}</span></Meta>}
      actions={<>
        <Button size="sm" variant="outline" onClick={() => setEdit(true)}>{t("people:contact.editInfo")}</Button>
        <Button variant="ghost" size="icon-sm" aria-label={c.important ? t("people:contact.unstar") : t("people:contact.star")} aria-pressed={c.important} className="text-muted-foreground" onClick={() => void act(people.patchContact(c.id, { important: !c.important }))} data-star-contact><StarIcon className={cn(c.important && "fill-current text-foreground")} /></Button>
        <DropdownMenu>
          <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label={t("common:action.more")} className="text-muted-foreground" data-contact-more />}><EllipsisIcon /></DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="min-w-44">
            <DropdownMenuItem onClick={() => setEdit(true)} data-edit-contact>{t("people:contact.editInfo")}</DropdownMenuItem>
            {!c.is_me && <DropdownMenuItem onClick={() => setMerge(true)} data-merge-contact>{t("people:contact.mergeOther")}</DropdownMenuItem>}
            {!c.is_me && <><DropdownMenuSeparator /><DropdownMenuItem variant="destructive" onClick={() => setDel(true)} data-del-contact>{t("common:action.delete")}</DropdownMenuItem></>}
          </DropdownMenuContent>
        </DropdownMenu></>}>
      <AlertDialog open={del} onOpenChange={setDel}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>{t("people:contact.delTitle", { name: c.name })}</AlertDialogTitle><AlertDialogDescription>{accts.data
          ? t("people:contact.delDescAccounts", { accounts: t("people:contact.accounts", { count: accts.data.length }), attrs: t("people:contact.attrs", { count: c.attrs.length }), mems: t("people:contact.mems", { count: mems.data?.length ?? 0 }) })
          : t("people:contact.delDesc", { attrs: t("people:contact.attrs", { count: c.attrs.length }), mems: t("people:contact.mems", { count: mems.data?.length ?? 0 }) })}</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" data-del-confirm onClick={async () => { if (await act(people.deleteContact(c.id), t("people:contact.deleted"))) go("#/contacts") }}>{t("common:action.delete")}</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
      {edit && <EditDialog c={c} onClose={() => setEdit(false)} onSaved={() => { setEdit(false); reload() }} />}
      {email && <EmailDialog id={c.id} onClose={() => setEmail(false)} onSaved={() => { setEmail(false); reload(); accts.reload() }} />}
      {job && <JobDialog c={c} onClose={() => setJob(false)} onSaved={() => { setJob(false); reload() }} />}
      {merge && <MergeDialog from={c} onClose={() => setMerge(false)} />}
      <div className="flex flex-col gap-8">
        {c.kind === "virtual" && <div className="flex flex-wrap items-center justify-between gap-2 rounded-xl bg-muted/60 px-4 py-3 type-callout"><span>{t("people:contact.virtualBanner")}</span><Button size="sm" variant="outline" onClick={async () => { if (await act(people.patchContact(c.id, { kind: "real" }), t("people:contact.restored"))) reload() }}>{t("people:contact.restore")}</Button></div>}
        {unconfirmed && <div className="flex flex-wrap items-center gap-3 rounded-xl bg-card px-5 py-4 shadow-[var(--lift)]" data-unconfirmed-banner>
          <div className="min-w-0 flex-1"><div className="type-headline">{t("people:contact.unconfirmedTitle")}</div><div className="mt-0.5 type-callout text-muted-foreground">{t("people:contact.unconfirmedSub")}</div></div>
          <Button size="sm" variant="outline" onClick={() => setMerge(true)}>{t("people:contact.mergeTo")}</Button>
          <Button size="sm" onClick={() => void act(people.confirm(c.id), t("people:contact.confirmedNew"))} data-confirm-contact>{t("people:contact.confirmNew")}</Button></div>}
        <div className="-mt-2 flex items-start gap-4"><ContactAvatar name={c.name} unconfirmed={unconfirmed} size={56} /><div className="min-w-0 flex-1"><div className="type-body whitespace-pre-wrap">{c.note || (c.is_me ? t("people:contact.isYou") : t("people:contact.noNote"))}</div><div className="mt-1 flex flex-wrap items-center gap-2"><Tags ids={c.tags} />
          {!c.is_me && <DropdownMenu><DropdownMenuTrigger render={<button type="button" className="type-footnote text-muted-foreground hover:text-foreground" data-rel-menu />}>{rels.length ? t("people:contact.changeRel") : t("people:contact.addRel")}</DropdownMenuTrigger>
            <DropdownMenuContent align="start" className="min-w-36">{codes("rel").filter((x) => x !== "me").map((x) => <DropdownMenuItem key={x} onClick={() => setRels(x)} data-rel-opt={x}>{c.rels.includes(x) ? <CheckIcon /> : <span className="size-4" />}{label("rel", x)}</DropdownMenuItem>)}</DropdownMenuContent></DropdownMenu>}</div></div></div>
        <AskSteve key={c.id} ctx={ctxOf("contact", c.id)} placeholder={c.is_me ? t("people:contact.askMe") : t("people:contact.askOther", { name: first(c.name) })} />
        {c.profile && Object.values(c.profile.fields).some((f) => f.state === "missing") && <Section label={t("people:contact.completeness")}><Group>{Object.entries(c.profile.fields).filter(([, f]) => f.state === "missing").map(([key, f]) => <Row key={key} title={key === "name" || key === "org" || key === "title" || key === "email" ? t(`people:contact.field.${key}`) : key} sub={profileReason(f)}><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => key === "name" ? setEdit(true) : key === "email" ? setEmail(true) : setJob(true)} data-fill-profile={key}><PlusIcon />{t("people:contact.fill")}</Button></Row>)}</Group>{c.profile.next_check && <p className="mt-3 type-footnote text-muted-foreground">{t("people:contact.nextCheck", { time: date(c.profile.next_check, { year: "numeric", month: "numeric", day: "numeric", hour: "numeric", minute: "2-digit" }) })}</p>}</Section>}
        <Section label={t("people:shared.info")} right={<Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setNewAttr({ label: "" })} data-add-attr><PlusIcon />{t("common:action.add")}</Button>}>
          <Group>
            {!c.is_me && <Row className={cn(STACKED, "sm:[&>[data-slot=item-actions]]:max-w-[60%] sm:[&>[data-slot=item-actions]]:shrink")} title={t("people:contact.orgTitle")} sub={past.length ? t("people:contact.before", { jobs: past.map((p) => t("people:contact.pastJob", { job: `${p.org_name}${p.title ? " · " + p.title : ""}`, to: p.to })).join(t("people:shared.clauses")) }) : undefined}><span className="min-w-0 flex-1 break-words type-callout">{now.map((j) => [j.org_name, j.dept, j.title].filter(Boolean).join(" · ")).join(t("people:shared.clauses")) || t("people:contact.notFilled")}</span><Button size="xs" variant="ghost" onClick={() => setJob(true)}>{t("people:contact.editJob")}</Button></Row>}
            {attrs.map((a) => <AttrRow key={a.id} a={a} onChanged={reload} />)}
            {missing.map(([k, l]) => <Row key={k} title={l} sub={t("people:contact.missingSub")}><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setNewAttr({ key: k, label: l })} data-fill-attr={k}><PlusIcon />{t("people:contact.fillIn")}</Button></Row>)}
            {newAttr && <NewAttr party={c.id} init={newAttr} onDone={() => { setNewAttr(null); reload() }} />}
          </Group>
          <div className="mt-3 type-footnote text-muted-foreground">{t("people:contact.levelNote")}</div>
        </Section>
        <Section label={t("people:contact.linked")} right={<Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setEmail(true)}><PlusIcon />{t("people:contact.addEmail")}</Button>}>{accts.data?.length ? <Group>{accts.data.map((a) => <Row key={a.id} title={a.display_name || a.system.name} sub={t("people:contact.accountSub", { system: a.system.name, source: a.source_label })}><span className="max-w-[50%] break-all type-callout text-muted-foreground">{a.handle}</span></Row>)}</Group> : <Empty title={t("people:contact.noLinked")} />}</Section>
        <HoldingsSection person={c.id} name={first(c.name)} isMe={c.is_me} isBot={c.kind === "bot"} />
        <Section label={t("people:contact.teams", { n: c.teams.length })}>{c.teams.length ? <Group>{c.teams.map((g) => <Row key={g.id} icon={<ContactAvatar name={g.name} type="team" size={28} />} title={g.name} sub={`${label("teamKind", g.kind)} · ${label("role", g.role)}`} onClick={() => go("#/teams/" + g.id)} data-contact-team={g.name}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Group> : <Empty title={t("people:contact.noTeams")} />}</Section>
        <Section label={t("people:shared.relatedItems", { n: items.data?.length ?? 0 })}>
          {!items.data ? <Loading /> : items.data.length ? <Group>{items.data.map((x) => { const i = st.items.find((y) => y.id === x.item_id); return (
            <Row key={x.item_id + x.role} title={<span><ItemNo id={x.item_id} className="mr-2" />{i?.t ?? x.title ?? ""}</span>} sub={<Meta inline><span>{label("itemRole", x.role)}</span>{x.confidence < 1 && <span>{t("people:shared.inferred")}</span>}</Meta>} onClick={() => go("#/items/" + x.item_id)} data-party-item={x.item_id}>{i && <Status s={itemDisplayStatus(i)} />}</Row>) })}</Group> : <Empty title={t("people:shared.noRelatedItems")} />}
        </Section>
        <Section label={t("people:contact.touches")}>
          {!tl.data ? <Loading /> : tl.data.length ? <div className="flex flex-col" data-touches>{tl.data.map((x, n) => { const I = TOUCH_I[x.kind] ?? ZapIcon; return (
            <button key={n} type="button" disabled={!x.href} onClick={() => x.href && go(x.href)} className="flex items-start gap-3 rounded-lg px-2 py-2 text-left transition-colors enabled:hover:bg-accent">
              <I className="mt-0.5 size-4 shrink-0 text-muted-foreground" strokeWidth={1.5} aria-hidden /><span className="min-w-0 flex-1 type-callout">{x.text}</span><span className="shrink-0 type-footnote tabular-nums text-muted-foreground">{label("touch", x.kind)} · {tm(x.ts)}</span></button>) })}</div> : <Empty title={t("people:contact.noTouches")} />}
        </Section>
        <MemSection party={c.id} mems={mems.data} label={c.is_me ? t("people:contact.remembersMe") : t("people:shared.steveRemembers")} />
        {c.merges.length > 0 && <Section label={t("people:contact.merges")}><Group>{c.merges.map((m) => (
          <Row key={m.id} title={t("people:contact.mergedFrom", { name: m.from_name })} sub={m.splittable ? t("people:contact.mergeSplittable", { day: day(m.created) }) : day(m.created)} data-merge-rec={m.id}>
            {m.splittable && <Button size="xs" variant="outline" onClick={() => void act(people.split(m.id), t("people:contact.splitDone"))} data-split={m.id}>{t("people:contact.split")}</Button>}</Row>))}</Group></Section>}
        {mates.length > 0 && <Section label={t("people:contact.mates")}><Group>{mates.map((x) => <Row key={x.id} icon={<ContactAvatar name={x.name} size={28} />} title={x.name} sub={x.title ?? undefined} onClick={() => go("#/contacts/" + x.id)}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Group></Section>}
      </div>
    </Page>
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

export function MemSection({ party, mems, label: heading, editable = true }: { party: string; mems: Mem[] | null; label: string; editable?: boolean }) {
  const t = useT()
  const [adding, setAdding] = useState(false), [text, setText] = useState("")
  const add = async () => { if (await act(people.addMemory(party, { text: text.trim() }))) { setText(""); setAdding(false) } }
  return (
    <Section label={`${heading} · ${mems?.length ?? 0}`} right={editable ? <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setAdding(true)} data-add-mem><PlusIcon />{t("common:action.add")}</Button> : undefined}>
      {adding && <div className="mb-2 flex items-center gap-2"><Input autoFocus className="h-8 flex-1 rounded-lg" aria-label={t("people:mem.newLabel")} placeholder={t("people:mem.newPh")} maxLength={2000} value={text} onChange={(e) => setText(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && text.trim() && !e.nativeEvent.isComposing) void add() }} data-mem-input />
        <Button size="sm" disabled={!text.trim()} onClick={() => void add()} data-mem-save>{t("people:mem.keep")}</Button><Button size="sm" variant="ghost" onClick={() => setAdding(false)}>{t("common:action.cancel")}</Button></div>}
      {!mems ? <Loading /> : mems.length ? <Group>{mems.map((m) => (
        <Row key={m.id} title={<span className="inline-flex items-center gap-2">{m.text}{m.status === "candidate" && <Badge variant="secondary" className="bg-attention/12 text-attention">{t("people:shared.pending")}</Badge>}{m.status === "stale" && <Badge variant="secondary" className="text-muted-foreground">{t("people:shared.stale")}</Badge>}</span>} sub={t("people:mem.sub", { kind: label("memKind", m.kind), source: m.source_label })} data-memo={m.text}>
          {m.status === "candidate" && <><Button size="xs" variant="outline" onClick={() => void act(people.patchMem(m.id, { status: "confirmed" }))} data-memo-ok>{t("common:action.confirm")}</Button><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => void act(people.deleteMem(m.id))}>{t("people:shared.wrong")}</Button></>}
          <LevelSel v={m.level} on={(l: Level) => void act(people.patchMem(m.id, { level: l }))} />
          {m.status !== "candidate" && <Button variant="ghost" size="icon-xs" aria-label={t("people:mem.del")} className="text-muted-foreground" onClick={() => void act(people.deleteMem(m.id))}><XIcon /></Button>}</Row>))}</Group>
        : <Empty title={t("people:mem.empty")} sub={t("people:mem.emptySub")} />}
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
function EmailDialog({ id, onClose, onSaved }: { id: string; onClose: () => void; onSaved: () => void }) {
  const t = useT()
  const [email, setEmail] = useState(""), [busy, setBusy] = useState(false)
  const save = async () => { setBusy(true); const r = await act(people.addAccount({ system_id: "mail", party_id: id, kind: "personal", handle: email.trim() }), t("people:email.added")); setBusy(false); if (r) onSaved() }
  return <Dialog open onOpenChange={(v) => !v && !busy && onClose()}><DialogContent><DialogHeader><DialogTitle>{t("people:contact.addEmail")}</DialogTitle><DialogDescription>{t("people:email.desc")}</DialogDescription></DialogHeader><form onSubmit={(e) => { e.preventDefault(); if (!busy) void save() }}><Input type="email" aria-label={t("people:email.address")} required value={email} onChange={(e) => setEmail(e.target.value)} /><DialogFooter className="mt-4"><Button type="button" variant="outline" disabled={busy} onClick={onClose}>{t("common:action.cancel")}</Button><Button type="submit" disabled={busy || !email.trim()}>{t("common:action.save")}</Button></DialogFooter></form></DialogContent></Dialog>
}
