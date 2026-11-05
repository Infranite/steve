/* 团队: the 团队 tab of #/contacts, #/teams/:id, the new-team dialog (prototype people.tsx TeamsTab, TeamRow, TeamView, NewTeamDialog) */
import { useEffect, useState } from "react"
import { ChevronRightIcon, PlusIcon, RefreshCwIcon, XIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Input } from "@/components/ui/input"
import { Checkbox } from "@/components/ui/checkbox"
import { Field, FieldGroup, FieldLabel } from "@/components/ui/field"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { st, tm } from "@/store"
import { modReq as req } from "@/api"
import { go, Page, Group, Row, SectionLabel, Empty, ItemNo, Status } from "@/ui"
import { Select } from "@/view-shared"
import { AskSteve } from "@/config-ui"
import { myContactId, people, type DirectoryStatus, type Team } from "@/api/people"
import { ContactAvatar, Loading, PeopleGate, Section, act, codes, label, useLoad, visLabel , SubMeta } from "@/people/common"
import { useT } from "@/i18n"
import { MemSection, ctxOf } from "@/people/contact"

export function TeamRow({ g, indent = 0, spaces }: { g: Team; indent?: number; spaces?: { id: string; name: string }[] | null }) {
  const t = useT()
  const sp = g.space_id ? spaces?.find((x) => x.id === g.space_id) : undefined
  return <Row icon={<span style={{ marginLeft: Math.min(indent, 6) * 20 }}><ContactAvatar name={g.name} type="team" size={32} /></span>} onClick={() => go("#/teams/" + g.id)} data-team-row={g.name}
    title={<span className="inline-flex items-center gap-1.5">{g.name}{g.visibility !== "workspace" && <span className="type-footnote text-muted-foreground">{visLabel(g.visibility)}</span>}</span>}
    sub={<SubMeta><span>{t("people:teams.headcount", { count: g.headcount })}</span>{sp && <span>{t("people:teams.bound", { name: sp.name })}</span>}{g.purpose && <span className="max-w-[320px] truncate">{g.purpose}</span>}</SubMeta>}><ChevronRightIcon className="size-4 text-tertiary" /></Row>
}
/** the channels module's groups; `null` when it is not installed (the bound-group parts hide) */
const loadSpaces = () => req<{ id: string; name: string }[]>("GET", "/spaces").catch(() => null)

export function TeamsTab() {
  const t = useT()
  const { data, err } = useLoad(() => people.teams(), [])
  const spaces = useLoad(loadSpaces, [], [])
  const [open, setOpen] = useState(false), [syncing, setSyncing] = useState(false)
  if (err) return <Empty title={t("people:teams.loadFailed")} sub={err.message} />
  if (!data) return <Loading />
  const sync = async () => { setSyncing(true); if (await act(people.syncTeams(), t("people:teams.syncStarted"))) { /* the result arrives as party events */ } setSyncing(false) }
  return (
    <div className="flex flex-col gap-8" data-teams>
      {open && <NewTeamDialog onClose={() => setOpen(false)} />}
      <section><SectionLabel right={<Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setOpen(true)} data-new-team><PlusIcon />{t("people:teams.new")}</Button>}>{t("people:teams.virtual", { n: data.virtual.length })}</SectionLabel>
        {data.virtual.length ? <Group>{data.virtual.map((g) => <TeamRow key={g.id} g={g} spaces={spaces.data} />)}</Group> : <Empty title={t("people:teams.noVirtual")} sub={t("people:teams.noVirtualSub")} />}
        <div className="mt-2 px-1 type-footnote text-muted-foreground">{t("people:teams.virtualNote")}</div></section>
      <section><SectionLabel right={<button type="button" disabled={syncing} className="inline-flex items-center gap-1 font-normal hover:text-foreground disabled:opacity-50" onClick={() => void sync()} aria-label={t("people:teams.sync")} data-sync><RefreshCwIcon className={syncing ? "size-3 animate-spin" : "size-3"} />{data.sync ? `${data.sync.label} · ${tm(data.sync.at)}` : t("people:teams.noSyncSource")}</button>}>{t("people:teams.chart")}</SectionLabel>
        <DirectoryNotes ds={data.directory} />
        {data.chart.length ? <Group>{data.chart.map((g) => <TeamRow key={g.id} g={g} indent={g.depth ?? 0} spaces={spaces.data} />)}</Group> : <Empty title={t("people:teams.noChart")} sub={t("people:teams.noChartSub")} />}
        <div className="mt-2 px-1 type-footnote text-muted-foreground">{t("people:teams.chartNote")}</div></section>
    </div>
  )
}

/** what the directory sync could not do or see: “应用可见范围只包含 N 个部门”, missing permissions (with a link to 接入) */
export function DirectoryNotes({ ds }: { ds?: DirectoryStatus[] }) {
  const t = useT()
  const xs = (ds ?? []).filter((d) => d.note)
  if (!xs.length) return null
  return <div className="mb-3 flex flex-col gap-1.5" role="note" data-directory-notes>{xs.map((d) => (
    <div key={d.connection_id} className="rounded-lg border border-attention/30 bg-attention/5 px-3 py-2 type-footnote text-foreground/80">
      <span className="font-medium">{d.label}</span>{t("people:teams.colon")}{d.note}{d.missing_scopes.length > 0 && <> · <a href="#/integrations" className="underline hover:text-foreground">{t("people:teams.openScopes")}</a></>}
    </div>))}</div>
}

export function TeamView({ id }: { id: string }) { return <PeopleGate>{() => <TeamPage id={id} />}</PeopleGate> }
function TeamPage({ id }: { id: string }) {
  const t = useT()
  const { data: g, err } = useLoad(() => people.team(id), [id])
  const all = useLoad(() => people.teams(), [])
  const items = useLoad(() => people.items(id), [id], ["party", "item"])
  const mems = useLoad(() => people.memories(id), [id])
  const spaces = useLoad(loadSpaces, [], [])
  const [del, setDel] = useState(false), [purpose, setPurpose] = useState<string | null>(null)
  useEffect(() => setPurpose(null), [g?.purpose])
  const back: [string, string] = ["#/contacts", t("people:shared.contacts")]
  if (err) return <Page title={t("people:team.title")} back={back}><Empty title={err.status === 404 ? t("people:team.notFound") : t("people:teams.loadFailed")} sub={err.status === 404 ? t("people:team.goneSub") : err.message} /></Page>
  if (!g) return <Page title={t("people:team.title")} back={back}><Loading /></Page>
  const v = g.kind === "virtual", members = g.members ?? []
  const teams = all.data ? [...all.data.chart, ...all.data.virtual] : []
  const parent = teams.find((x) => x.id === g.parent_id), subs = teams.filter((x) => g.children.includes(x.id))
  const put = (ms: { contact_id: string; role: string }[]) => void act(people.putMembers(g.id, ms))
  const cur = members.map((m) => ({ contact_id: m.contact_id, role: m.role }))
  const savePurpose = () => { if (purpose !== null && purpose !== g.purpose) void act(people.patchTeam(g.id, { purpose: purpose.slice(0, 400) })) }
  return (
    <Page title={g.name} back={back} max={720} sub={<SubMeta><span>{label("teamKind", g.kind)}</span><span>{g.source_label}</span>{parent && <a href={"#/teams/" + parent.id} className="hover:text-foreground">{t("people:team.parent", { name: parent.name })}</a>}<span>{t("people:teams.headcount", { count: g.headcount })}</span>{g.status === "dissolved" && <span>{t("people:team.dissolved")}</span>}</SubMeta>}
      actions={v && g.status === "active" ? <Button size="sm" variant="ghost" className="text-destructive" onClick={() => setDel(true)} data-del-team>{t("people:team.dissolve")}</Button> : undefined}>
      <AlertDialog open={del} onOpenChange={setDel}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>{t("people:team.dissolveTitle", { name: g.name })}</AlertDialogTitle><AlertDialogDescription>{t("people:team.dissolveDesc")}</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" data-del-team-confirm onClick={async () => { if (await act(people.dissolve(g.id), t("people:team.dissolved"))) go("#/contacts") }}>{t("people:team.dissolve")}</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
      <div className="flex flex-col gap-8">
        <AskSteve key={g.id} ctx={ctxOf("team", g.id)} placeholder={v ? t("people:team.askVirtual", { name: g.name }) : t("people:team.askChart", { name: g.name })} />
        <Section label={t("people:team.purpose")}><Input className="rounded-lg" aria-label={t("people:team.purposeAria")} maxLength={400} value={purpose ?? g.purpose} onChange={(e) => setPurpose(e.target.value)} onBlur={savePurpose} onKeyDown={(e) => { if (e.key === "Enter" && !e.nativeEvent.isComposing) (e.target as HTMLInputElement).blur() }} placeholder={t("people:team.purposePh")} data-team-purpose /></Section>
        <Section label={t("people:team.members", { n: members.length })} right={v ? <AddMember have={members.map((m) => m.contact_id)} onPick={(cid) => put([...cur, { contact_id: cid, role: "member" }])} /> : undefined}>
          {members.length ? <Group>{members.map((m) => (
            <Row key={m.contact_id} icon={<ContactAvatar name={m.name} size={28} />} title={<span className="inline-flex items-center gap-1.5"><a href={"#/contacts/" + m.contact_id} className="hover:underline">{m.name}</a>{m.external && <Badge variant="outline" className="font-normal text-muted-foreground">{t("people:team.external")}</Badge>}</span>} data-member={m.name}>
              {v && m.source !== "sync" ? <><span><Select align="end" quiet testId={"role-" + m.name} value={m.role} onChange={(x) => put(cur.map((c) => (c.contact_id === m.contact_id ? { ...c, role: x } : c)))} options={codes("role").map((x) => ({ v: x, l: <span className="type-footnote">{label("role", x)}</span> }))} /></span>
                <Button variant="ghost" size="icon-xs" aria-label={t("people:team.removeMember", { name: m.name })} className="text-muted-foreground" onClick={() => put(cur.filter((c) => c.contact_id !== m.contact_id))} data-rm-member={m.name}><XIcon /></Button></>
                : <span className="type-footnote text-muted-foreground">{label("role", m.role)}</span>}</Row>))}</Group> : <Empty title={t("people:team.noMembers")} />}
          {!v && <div className="mt-2 px-1 type-footnote text-muted-foreground">{t("people:team.syncedMembers")}</div>}
        </Section>
        {subs.length > 0 && <Section label={t("people:team.subs")}><Group>{subs.map((x) => <TeamRow key={x.id} g={x} spaces={spaces.data} />)}</Group></Section>}
        {spaces.data && <Section label={t("people:team.space")}><Group><Row title={spaces.data.find((x) => x.id === g.space_id)?.name ?? t("people:team.noSpace")} sub={t("people:team.spaceSub")}>
          <Select align="end" testId="team-space" value={g.space_id ?? "none"} onChange={(x) => void act(people.patchTeam(g.id, { space_id: x === "none" ? null : x }))} options={[{ v: "none", l: t("people:team.unbind") }, ...spaces.data.map((x) => ({ v: x.id, l: x.name }))]} /></Row></Group></Section>}
        <Section label={t("people:team.items", { n: items.data?.length ?? 0 })}>{!items.data ? <Loading /> : items.data.length ? <Group>{[...new Map(items.data.map((x) => [x.item_id, x])).values()].map((x) => { const i = st.items.find((y) => y.id === x.item_id); return <Row key={x.item_id} title={<span><ItemNo id={x.item_id} className="mr-2" />{i?.t ?? x.title ?? ""}</span>} sub={label("itemRole", x.role)} onClick={() => go("#/items/" + x.item_id)}>{i && <Status s={i.st} />}</Row> })}</Group> : <Empty title={t("people:shared.noRelatedItems")} />}</Section>
        <MemSection party={g.id} mems={mems.data} label={t("people:shared.steveRemembers")} />
        <div className="px-1 type-footnote text-muted-foreground">{t("people:team.visNote", { vis: visLabel(g.visibility), name: g.name })}</div>
      </div>
    </Page>
  )
}

function AddMember({ have, onPick }: { have: string[]; onPick: (cid: string) => void }) {
  const t = useT()
  const [open, setOpen] = useState(false)
  const cs = useLoad(() => (open ? people.contacts({ limit: 500 }) : Promise.resolve(null)), [open])
  const xs = (cs.data ?? []).filter((c) => !have.includes(c.id))
  return (
    <DropdownMenu open={open} onOpenChange={setOpen}>
      <DropdownMenuTrigger render={<Button size="xs" variant="ghost" className="text-muted-foreground" data-add-member />}><PlusIcon />{t("common:action.add")}</DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="max-h-72 min-w-56">
        {!cs.data ? <div className="px-2 py-1.5 type-footnote text-muted-foreground">{t("people:shared.loading")}</div> : !xs.length ? <div className="px-2 py-1.5 type-footnote text-muted-foreground">{t("people:team.noCandidates")}</div>
          : xs.map((c) => <DropdownMenuItem key={c.id} onClick={() => onPick(c.id)} data-member-opt={c.name}><ContactAvatar name={c.name} size={18} />{c.is_me ? t("people:shared.withMe", { name: c.name }) : c.name}<span className="ml-auto type-footnote text-muted-foreground">{c.org?.name}</span></DropdownMenuItem>)}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

export function NewTeamDialog({ onClose }: { onClose: () => void }) {
  const t = useT()
  const me = myContactId()
  const [n, setN] = useState(""), [pur, setPur] = useState(""), [pick, setPick] = useState<string[]>(me ? [me] : []), [busy, setBusy] = useState(false)
  const cs = useLoad(() => people.contacts({ limit: 500 }), [])
  const xs = (cs.data ?? []).filter((c) => c.kind !== "unconfirmed").sort((a, b) => Number(b.is_me) - Number(a.is_me))
  const create = async () => {
    setBusy(true)
    const g = await act(people.newTeam({ name: n.trim(), purpose: pur.trim() || undefined, members: pick.map((c) => ({ contact_id: c })) }))
    setBusy(false); if (g) { onClose(); go("#/teams/" + g.id) }
  }
  return (
    <Dialog open onOpenChange={(x) => !x && onClose()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[480px]">
        <DialogHeader><DialogTitle className="type-headline">{t("people:newTeam.title")}</DialogTitle><DialogDescription className="type-callout">{t("people:newTeam.desc")}</DialogDescription></DialogHeader>
        <FieldGroup className="gap-4">
          <Field><FieldLabel htmlFor="nt-name">{t("people:newTeam.name")}</FieldLabel><Input id="nt-name" autoFocus className="rounded-lg" maxLength={40} value={n} onChange={(e) => setN(e.target.value)} placeholder={t("people:newTeam.namePh")} data-nt-name /></Field>
          <Field><FieldLabel htmlFor="nt-purpose">{t("people:newTeam.purpose")}</FieldLabel><Input id="nt-purpose" className="rounded-lg" maxLength={400} value={pur} onChange={(e) => setPur(e.target.value)} placeholder={t("people:newTeam.purposePh")} /></Field>
          <Field><FieldLabel>{t("people:team.members", { n: pick.length })}</FieldLabel>
            <div className="max-h-52 overflow-y-auto rounded-lg border" role="group" aria-label={t("people:newTeam.pick")}>{!cs.data ? <Loading /> : xs.map((c) => (
              <label key={c.id} className="flex cursor-pointer items-center gap-2.5 px-3 py-2 hover:bg-accent" data-nt-pick={c.name}><Checkbox checked={pick.includes(c.id)} onCheckedChange={(v) => setPick(v ? [...pick, c.id] : pick.filter((x) => x !== c.id))} /><ContactAvatar name={c.name} size={20} /><span className="type-callout">{c.is_me ? t("people:shared.withMe", { name: c.name }) : c.name}</span><span className="ml-auto type-footnote text-muted-foreground">{c.org?.name}</span></label>))}</div></Field>
        </FieldGroup>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button><Button disabled={!n.trim() || pick.length < 2 || busy} data-testid="nt-go" onClick={() => void create()}>{t("people:shared.create")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
