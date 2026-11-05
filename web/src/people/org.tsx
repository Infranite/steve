/* #/orgs/:id — one organization (prototype people.tsx OrgView) */
import { useState } from "react"
import { ChevronRightIcon, PlusIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { st } from "@/store"
import { go, Page, Group, Row, Empty, ItemNo, Status } from "@/ui"
import { AskSteve } from "@/config-ui"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { people, type Org } from "@/api/people"
import { ContactAvatar, Loading, PeopleGate, Section, act, codes, joinList, label, useLoad , SubMeta } from "@/people/common"
import { useT } from "@/i18n"
import { AttrRow, MemSection, NewAttr, ctxOf } from "@/people/contact"

export function OrgView({ id }: { id: string }) { return <PeopleGate>{() => <OrgPage id={id} />}</PeopleGate> }
function OrgPage({ id }: { id: string }) {
  const t = useT()
  const { data: o, err, reload } = useLoad(() => people.org(id), [id])
  const items = useLoad(() => people.items(id), [id], ["party", "item"])
  const mems = useLoad(() => people.memories(id), [id])
  const [newAttr, setNewAttr] = useState(false), [edit, setEdit] = useState(false)
  const back: [string, string] = ["#/contacts", t("people:shared.contacts")]
  if (err) return <Page title={t("people:shared.org")} back={back}><Empty title={err.status === 404 ? t("people:org.notFound") : t("people:shared.orgLoadFailed")} sub={err.status === 404 ? undefined : err.message} /></Page>
  if (!o) return <Page title={t("people:shared.org")} back={back}><Loading /></Page>
  const ps = o.people ?? []
  return (
    <Page actions={<Button size="sm" variant="outline" onClick={() => setEdit(true)}>{t("people:org.edit")}</Button>} title={o.name} back={back} max={720} sub={<SubMeta>{o.kind && <span>{label("orgKind", o.kind)}</span>}<span>{joinList(o.domains) || t("people:shared.noDomain")}</span>{o.aliases.length > 0 && <span>{t("people:shared.aka", { names: joinList(o.aliases) })}</span>}</SubMeta>}>
      {edit && <EditOrg o={o} onClose={() => setEdit(false)} onSaved={() => { setEdit(false); void reload() }} />}
      <div className="flex flex-col gap-8">
        {(o.name_state === "placeholder" || o.name_state === "workspace") && <div className="rounded-lg border p-3 text-sm"><p>{o.name_reason === "external_unavailable" ? t("people:org.externalName") : o.name_reason === "permission" ? (o.name_scope ? t("people:org.noPermissionScope", { scope: o.name_scope }) : t("people:org.noPermission")) : t("people:org.incomplete")}</p><Button size="sm" variant="outline" className="mt-2" onClick={() => setEdit(true)}>{t("people:org.name")}</Button></div>}
        {o.tenant && <div className="break-all text-xs text-muted-foreground">{t("people:org.tenant", { key: o.tenant.key })}</div>}
        {o.note && <p className="-mt-2 whitespace-pre-wrap type-body text-muted-foreground">{o.note}</p>}
        <AskSteve key={o.id} ctx={ctxOf("org", o.id)} placeholder={t("people:org.ask", { name: o.name })} />
        <Section label={t("people:org.people", { n: ps.length })}>{ps.length ? <Group>{ps.map((c) => <Row key={c.id} icon={<ContactAvatar name={c.name} size={28} />} title={c.name} sub={c.title ?? undefined} onClick={() => go("#/contacts/" + c.id)} data-org-person={c.name}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Group> : <Empty title={t("people:shared.noContacts")} />}</Section>
        <Section label={t("people:shared.info")} right={<Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setNewAttr(true)} data-add-attr><PlusIcon />{t("common:action.add")}</Button>}>
          <Group>
            <Row title={t("people:org.domains")} sub={t("people:org.domainsSub")}><span className="type-callout">{joinList(o.domains) || "—"}</span></Row>
            {o.attrs.map((a) => <AttrRow key={a.id} a={a} onChanged={() => void reload()} />)}
            {newAttr && <NewAttr party={o.id} init={{ label: "" }} onDone={() => { setNewAttr(false); void reload() }} />}
          </Group>
        </Section>
        <Section label={t("people:shared.relatedItems", { n: items.data?.length ?? 0 })}>{!items.data ? <Loading /> : items.data.length ? <Group>{items.data.map((x) => { const i = st.items.find((y) => y.id === x.item_id); return <Row key={x.item_id + x.role} title={<span><ItemNo id={x.item_id} className="mr-2" />{i?.t ?? x.title ?? ""}</span>} sub={label("itemRole", x.role)} onClick={() => go("#/items/" + x.item_id)}>{i && <Status s={i.st} />}</Row> })}</Group> : <Empty title={t("people:shared.noRelatedItems")} />}</Section>
        <MemSection party={o.id} mems={mems.data} label={t("people:shared.steveRemembers")} />
      </div>
    </Page>
  )
}

function EditOrg({ o, onClose, onSaved }: { o: Org; onClose: () => void; onSaved: () => void }) {
  const t = useT()
  const [name, setName] = useState(o.name), [aliases, setAliases] = useState(joinList(o.aliases)), [domains, setDomains] = useState(joinList(o.domains)), [kind, setKind] = useState(o.kind ?? "other"), [note, setNote] = useState(o.note), [busy, setBusy] = useState(false)
  const split = (s: string) => [...new Set(s.split(/[、,，\s]+/).filter(Boolean))]
  const save = async () => {
    setBusy(true)
    const r = await act(people.patchOrg(o.id, { name: name.trim(), aliases: split(aliases), domains: split(domains), note, ...(o.kind === "self" ? {} : { kind }) }), t("people:editOrg.saved"))
    setBusy(false); if (r) onSaved()
  }
  return <Dialog open onOpenChange={(v) => !v && !busy && onClose()}><DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-[460px]">
    <DialogHeader><DialogTitle>{t("people:org.edit")}</DialogTitle><DialogDescription>{t("people:editOrg.desc")}</DialogDescription></DialogHeader>
    <label className="space-y-1 text-sm">{t("people:editOrg.name")}<Input aria-label={t("people:editOrg.name")} value={name} maxLength={80} onChange={(e) => setName(e.target.value)} /></label>
    <label className="space-y-1 text-sm">{t("people:editOrg.aliases")}<Input aria-label={t("people:editOrg.aliasesAria")} value={aliases} placeholder={t("people:editOrg.aliasesPh")} onChange={(e) => setAliases(e.target.value)} /></label>
    <label className="space-y-1 text-sm">{t("people:org.domains")}<Input aria-label={t("people:editOrg.domainsAria")} value={domains} placeholder={t("people:editOrg.domainsPh")} onChange={(e) => setDomains(e.target.value)} /></label>
    <label className="space-y-1 text-sm">{t("people:editOrg.kind")}<select aria-label={t("people:editOrg.kindAria")} className="block h-9 w-full rounded-md border bg-background px-2" disabled={o.kind === "self" || busy} value={kind} onChange={(e) => setKind(e.target.value)}>{codes("orgKind").filter((k) => k !== "self" || o.kind === "self").map((k) => <option key={k} value={k}>{label("orgKind", k)}</option>)}</select></label>
    <label className="space-y-1 text-sm">{t("people:shared.note")}<Textarea aria-label={t("people:editOrg.noteAria")} value={note} maxLength={2000} onChange={(e) => setNote(e.target.value)} /></label>
    <DialogFooter><Button variant="outline" disabled={busy} onClick={onClose}>{t("common:action.cancel")}</Button><Button disabled={busy || !name.trim()} onClick={() => void save()}>{t("common:action.save")}</Button></DialogFooter>
  </DialogContent></Dialog>
}
