/* #/contacts — 联系人、团队、组织、待确认 (prototype people.tsx ContactsView, NewContactDialog) */
import { useEffect, useState, type ReactNode } from "react"
import { ChevronRightIcon, PlusIcon, StarIcon, UserRoundIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Input } from "@/components/ui/input"
import { Field, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { go, Page, Group, Row, Segmented, SectionLabel, Empty, Chips, Tags, Meta, SearchField } from "@/ui"
import { Select } from "@/view-shared"
import { AskSteve } from "@/config-ui"
import { myContactId, nameOf, people, type ContactBrief } from "@/api/people"
import { ContactAvatar, Loading, PeopleGate, act, codes, joinList, label, relLabel, useLoad } from "@/people/common"
import { t as tr, useT } from "@/i18n"
import { DirectoryNotes, TeamRow, TeamsTab } from "@/people/teams"
import { PendingContacts } from "@/people/pending"
import { UnownedSection } from "@/people/holdings"

type Tab = "contacts" | "teams" | "orgs" | "pending"
type Filter = "all" | "colleague" | "customer" | "partner" | "virtual"
/* the tab, filter and search survive leaving the page and coming back, as in the prototype */
let ctab: Tab = "contacts", crel: Filter = "all", cq = ""
const FILTERS: Record<Filter, { rel?: string; kind?: string }> = { all: {}, colleague: { rel: "colleague" }, customer: { rel: "customer" }, partner: { rel: "partner" }, virtual: { kind: "virtual" } }
const FILTER_KEYS = Object.keys(FILTERS) as Filter[]
/** “公司 · 部门路径 · 职位” */
export const orgLine = (c: Pick<ContactBrief, "org">) => [c.org?.name, c.org?.dept, c.org?.title].filter(Boolean).join(" · ")

export function ContactsView() { return <PeopleGate>{() => <Contacts />}</PeopleGate> }
function Contacts() {
  const t = useT()
  const [tab, setTab] = useState<Tab>(ctab), [rel, setRel] = useState<Filter>(crel), [q, setQ] = useState(cq), [dq, setDq] = useState(cq), [newOpen, setNewOpen] = useState(false)
  useEffect(() => { const h = setTimeout(() => setDq(q.trim()), 200); return () => clearTimeout(h) }, [q])
  const pend = useLoad(() => people.pending(), [])
  const n = pend.data ? pend.data.proposals.length + pend.data.unconfirmed.length : 0
  return (
    <Page title={t("people:shared.contacts")} sub={t("people:contacts.sub")} max={760}
      actions={<><Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => go("#/contacts/me")} data-my-contact><UserRoundIcon />{t("people:contacts.me")}</Button>
        <Button size="sm" onClick={() => setNewOpen(true)} data-new-contact><PlusIcon />{t("people:contacts.new")}</Button></>}
      toolbar={<Segmented value={tab} onChange={(v) => { ctab = v; setTab(v) }} options={[["contacts", t("people:contacts.tab.contacts")], ["teams", t("people:contacts.tab.teams")], ["orgs", t("people:contacts.tab.orgs")],
        ["pending", <span key="p" className="inline-flex items-center gap-1">{t("people:contacts.tab.pending")}{n > 0 && <span className="rounded-full bg-attention/14 px-1.5 type-label tabular-nums text-attention" aria-label={t("people:contacts.pendingCount", { count: n })} data-pending-count>{n}</span>}</span>, t("people:contacts.tab.pending")]] as const} />}>
      {newOpen && <NewContactDialog onClose={() => setNewOpen(false)} />}
      <AskSteve className="mb-6" ctx={{ kind: "global" }} placeholder={t("people:contacts.ask")} />
      <div key={tab} className="anim-in" data-contacts-tab={tab}>
      {tab === "contacts" && <>
        <ContactList q={dq} f={FILTERS[rel] ?? {}} toolbar={<div className="mb-5 flex flex-wrap items-center gap-2"><Chips value={filterLabel(rel)} onChange={(v) => { const f = FILTER_KEYS.find((k) => filterLabel(k) === v) ?? "all"; crel = f; setRel(f) }} options={FILTER_KEYS.map(filterLabel)} className="flex-wrap" /><span className="flex-1" />
          <SearchField size="sm" className="w-full sm:w-60" aria-label={t("people:shared.searchContacts")} placeholder={t("people:shared.searchContactsPh")} value={q} onChange={(e) => { cq = e.target.value; setQ(e.target.value) }} data-contact-search /></div>} />
        {!dq && rel === "all" && <UnownedSection />}
      </>}
      {tab === "teams" && <TeamsTab />}
      {tab === "orgs" && <OrgList />}
      {tab === "pending" && <PendingContacts />}
      </div>
    </Page>
  )
}

/** a filter chip's text: the chips show their labels, so the filter code is mapped to and from it */
const filterLabel = (f: Filter) => (f === "all" ? tr("people:contacts.all") : f === "virtual" ? label("kind", f) : label("rel", f))

/** who someone is in a few words: their title at their organization (the department path is on their page) */
const whoLine = (c: ContactBrief) => [c.org?.title, c.org?.name].filter(Boolean).join(" · ")
function ContactList({ q, f, toolbar }: { q: string; f: { rel?: string; kind?: string }; toolbar: ReactNode }) {
  const t = useT()
  const { data, err } = useLoad(() => people.contacts({ q, ...f }), [q, f.rel, f.kind])
  if (err) return <Empty title={t("people:contact.loadFailed")} sub={err.message} />
  if (!data) return <Loading />
  const xs = data.filter((c) => !c.is_me), filtered = !!(q || f.rel || f.kind)
  /* nobody yet: no filters and no search over an empty list, only how people get here */
  if (!xs.length && !filtered) return <Empty title={t("people:shared.noContacts")} sub={t("people:contacts.emptySub")} />
  return <>{toolbar}{!xs.length ? <Empty title={t("people:shared.noMatch")} /> : (
    <Group>{xs.map((c) => (
      <Row key={c.id} icon={<ContactAvatar name={nameOf(c)} avatar={c.avatar} size={36} />} onClick={() => go("#/contacts/" + c.id)} data-contact-row={c.name}
        title={<span className="inline-flex items-center gap-1.5">{nameOf(c)}{c.important && <StarIcon className="size-3.5 fill-current text-tertiary" aria-label={t("people:contacts.important")} />}{c.kind === "virtual" && <Badge variant="secondary" className="text-muted-foreground">{label("kind", "virtual")}</Badge>}{c.kind === "bot" && <Badge variant="secondary" className="text-muted-foreground">{label("kind", "bot")}</Badge>}</span>}
        sub={<Meta inline>{whoLine(c) && <span className="truncate">{whoLine(c)}</span>}{c.rels.filter((r) => r !== "me").length > 0 && <span>{joinList(c.rels.filter((r) => r !== "me").map(relLabel))}</span>}{!whoLine(c) && !c.rels.some((r) => r !== "me") && <span>{t("people:contacts.accounts", { count: c.accounts })}</span>}</Meta>}>
        <Tags ids={c.tags} max={2} />{c.items > 0 && <span className="hidden shrink-0 type-footnote tabular-nums text-muted-foreground sm:inline" data-contact-items>{t("people:contacts.items", { count: c.items })}</span>}<ChevronRightIcon className="size-4 text-tertiary" /></Row>))}</Group>)}</>
}

function OrgList() {
  const t = useT()
  const { data, err } = useLoad(() => people.orgs(), [])
  const teams = useLoad(() => people.teams().catch(() => null), [])
  if (err) return <Empty title={t("people:shared.orgLoadFailed")} sub={err.message} />
  if (!data) return <Loading />
  if (!data.length) return <Empty title={t("people:orgs.empty")} sub={t("people:orgs.emptySub")} />
  const chart = teams.data?.chart.filter((g) => g.status === "active") ?? [], own = data.find((o) => o.kind === "self")
  return (
    <div className="flex flex-col gap-8">
      <Group>{data.map((o) => (
        <Row key={o.id} icon={<ContactAvatar name={o.name} type="org" size={36} />} onClick={() => go("#/orgs/" + o.id)} data-org-row={o.name} title={o.name}
          sub={<Meta inline>{o.kind && <span>{label("orgKind", o.kind)}</span>}<span>{joinList(o.domains) || t("people:shared.noDomain")}</span>{(o.name_state === "placeholder" || o.name_state === "workspace") && <span>{t("people:orgs.unnamed")}</span>}</Meta>}><ChevronRightIcon className="size-4 text-tertiary" /></Row>))}</Group>
      {teams.data && (chart.length > 0 || (teams.data.directory?.length ?? 0) > 0) && <section data-dept-tree>
        <SectionLabel>{own ? t("people:orgs.deptsOf", { name: own.name, n: chart.length }) : t("people:orgs.depts", { n: chart.length })}</SectionLabel>
        <DirectoryNotes ds={teams.data.directory} />
        {chart.length ? <Group>{chart.map((g) => <TeamRow key={g.id} g={g} indent={g.depth ?? 0} />)}</Group> : <Empty title={t("people:orgs.noDepts")} sub={t("people:orgs.noDeptsSub")} />}
      </section>}
    </div>
  )
}

export function NewContactDialog({ onClose }: { onClose: () => void }) {
  const t = useT()
  const [n, setN] = useState(""), [rel, setRel] = useState("colleague"), [o, setO] = useState(""), [title, setTitle] = useState(""), [mail, setMail] = useState(""), [busy, setBusy] = useState(false)
  const orgs = useLoad(() => people.orgs(), [])
  /* the mail field is an account of the accounts module: only offered when that module answers */
  const accts = useLoad(() => people.accounts(myContactId() ?? "me").then(() => true, () => false), [])
  const create = async () => {
    setBusy(true)
    try {
      const c = await act(people.newContact({ name: n.trim(), rels: rel ? [rel] : [], org_id: o || undefined, title: o && title.trim() ? title.trim() : undefined }))
      if (!c) return
      if (mail.trim()) await act(people.addAccount({ system_id: "mail", party_id: c.id, kind: "personal", handle: mail.trim() }))
      onClose(); go("#/contacts/" + c.id)
    } finally { setBusy(false) }
  }
  return (
    <Dialog open onOpenChange={(x) => !x && onClose()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[460px]">
        <DialogHeader><DialogTitle className="type-headline">{t("people:contacts.new")}</DialogTitle><DialogDescription className="type-callout">{t("people:newContact.desc")}</DialogDescription></DialogHeader>
        <FieldGroup className="gap-4">
          <Field><FieldLabel htmlFor="nc-name">{t("people:shared.name")}</FieldLabel><Input id="nc-name" autoFocus className="rounded-lg" maxLength={80} value={n} onChange={(e) => setN(e.target.value)} data-nc-name /></Field>
          <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("people:newContact.rel")}</FieldLabel><Select testId="nc-rel" value={rel} onChange={setRel} options={[{ v: "", l: t("people:newContact.noRel") }, ...codes("rel").filter((r) => r !== "me").map((v) => ({ v, l: label("rel", v) }))]} /></Field>
          <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("people:shared.org")}</FieldLabel><Select testId="nc-org" value={o} onChange={setO} options={[{ v: "", l: t("common:state.none") }, ...(orgs.data ?? []).map((x) => ({ v: x.id, l: x.name }))]} /></Field>
          {o && <Field><FieldLabel htmlFor="nc-title">{t("people:newContact.title")}</FieldLabel><Input id="nc-title" className="rounded-lg" maxLength={80} value={title} onChange={(e) => setTitle(e.target.value)} data-nc-title /></Field>}
          {accts.data && <Field><FieldLabel htmlFor="nc-mail">{t("people:newContact.email")}</FieldLabel><Input id="nc-mail" type="email" className="rounded-lg" value={mail} onChange={(e) => setMail(e.target.value)} placeholder={t("people:newContact.emailPh")} data-nc-mail /></Field>}
        </FieldGroup>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button><Button disabled={!n.trim() || busy} data-testid="nc-go" onClick={() => void create()}>{t("people:shared.create")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
