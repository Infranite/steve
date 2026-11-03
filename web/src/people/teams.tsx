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
import { ContactAvatar, ITEM_ROLE, Loading, PeopleGate, ROLE, Section, TEAM_KIND, act, useLoad, visLabel , SubMeta } from "@/people/common"
import { MemSection, ctxOf } from "@/people/contact"

export function TeamRow({ g, indent = 0, spaces }: { g: Team; indent?: number; spaces?: { id: string; name: string }[] | null }) {
  const sp = g.space_id ? spaces?.find((x) => x.id === g.space_id) : undefined
  return <Row icon={<span style={{ marginLeft: Math.min(indent, 6) * 20 }}><ContactAvatar name={g.name} type="team" size={32} /></span>} onClick={() => go("#/teams/" + g.id)} data-team-row={g.name}
    title={<span className="inline-flex items-center gap-1.5">{g.name}{g.visibility !== "workspace" && <span className="type-footnote text-muted-foreground">{visLabel(g.visibility)}</span>}</span>}
    sub={<SubMeta><span>{g.headcount} 人</span>{sp && <span>绑定 {sp.name}</span>}{g.purpose && <span className="max-w-[320px] truncate">{g.purpose}</span>}</SubMeta>}><ChevronRightIcon className="size-4 text-tertiary" /></Row>
}
/** the channels module's groups; `null` when it is not installed (the bound-group parts hide) */
const loadSpaces = () => req<{ id: string; name: string }[]>("GET", "/spaces").catch(() => null)

export function TeamsTab() {
  const { data, err } = useLoad(() => people.teams(), [])
  const spaces = useLoad(loadSpaces, [], [])
  const [open, setOpen] = useState(false), [syncing, setSyncing] = useState(false)
  if (err) return <Empty title="团队载入失败" sub={err.message} />
  if (!data) return <Loading />
  const sync = async () => { setSyncing(true); if (await act(people.syncTeams(), "已开始同步组织架构")) { /* the result arrives as party events */ } setSyncing(false) }
  return (
    <div className="flex flex-col gap-8" data-teams>
      {open && <NewTeamDialog onClose={() => setOpen(false)} />}
      <section><SectionLabel right={<Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setOpen(true)} data-new-team><PlusIcon />新建</Button>}>虚拟团队 · {data.virtual.length}</SectionLabel>
        {data.virtual.length ? <Group>{data.virtual.map((g) => <TeamRow key={g.id} g={g} spaces={spaces.data} />)}</Group> : <Empty title="还没有虚拟团队" sub="兴趣小组、跨部门协作、和客户一起的交付小组，都可以建一个。" />}
        <div className="mt-2 px-1 type-footnote text-muted-foreground">自己组建，可以包含外部联系人。也可以对 Steve 说“把李雷、王芳和 Sarah 拉一个 Acme 交付小组”。</div></section>
      <section><SectionLabel right={<button type="button" disabled={syncing} className="inline-flex items-center gap-1 font-normal hover:text-foreground disabled:opacity-50" onClick={() => void sync()} aria-label="同步组织架构" data-sync><RefreshCwIcon className={syncing ? "size-3 animate-spin" : "size-3"} />{data.sync ? `${data.sync.label} · ${tm(data.sync.at)}` : "没有同步源"}</button>}>组织架构</SectionLabel>
        <DirectoryNotes ds={data.directory} />
        {data.chart.length ? <Group>{data.chart.map((g) => <TeamRow key={g.id} g={g} indent={g.depth ?? 0} spaces={spaces.data} />)}</Group> : <Empty title="没有同步组织架构" sub="接入办公 IM 并开启“读通讯录”后，部门和上下级会同步到这里。" />}
        <div className="mt-2 px-1 type-footnote text-muted-foreground">从办公 IM 的通讯录同步，成员和上下级只读；可以在本地加说明、标签和绑定的群。</div></section>
    </div>
  )
}

/** what the directory sync could not do or see: “应用可见范围只包含 N 个部门”, missing permissions (with a link to 接入) */
export function DirectoryNotes({ ds }: { ds?: DirectoryStatus[] }) {
  const xs = (ds ?? []).filter((d) => d.note)
  if (!xs.length) return null
  return <div className="mb-3 flex flex-col gap-1.5" role="note" data-directory-notes>{xs.map((d) => (
    <div key={d.connection_id} className="rounded-lg border border-attention/30 bg-attention/5 px-3 py-2 type-footnote text-foreground/80">
      <span className="font-medium">{d.label}</span>：{d.note}{d.missing_scopes.length > 0 && <> · <a href="#/integrations" className="underline hover:text-foreground">去接入页开通</a></>}
    </div>))}</div>
}

export function TeamView({ id }: { id: string }) { return <PeopleGate>{() => <TeamPage id={id} />}</PeopleGate> }
function TeamPage({ id }: { id: string }) {
  const { data: g, err } = useLoad(() => people.team(id), [id])
  const all = useLoad(() => people.teams(), [])
  const items = useLoad(() => people.items(id), [id], ["party", "item"])
  const mems = useLoad(() => people.memories(id), [id])
  const spaces = useLoad(loadSpaces, [], [])
  const [del, setDel] = useState(false), [purpose, setPurpose] = useState<string | null>(null)
  useEffect(() => setPurpose(null), [g?.purpose])
  if (err) return <Page title="团队" back={["#/contacts", "联系人"]}><Empty title={err.status === 404 ? "团队不存在" : "团队载入失败"} sub={err.status === 404 ? "可能已经解散。" : err.message} /></Page>
  if (!g) return <Page title="团队" back={["#/contacts", "联系人"]}><Loading /></Page>
  const v = g.kind === "virtual", members = g.members ?? []
  const teams = all.data ? [...all.data.chart, ...all.data.virtual] : []
  const parent = teams.find((x) => x.id === g.parent_id), subs = teams.filter((x) => g.children.includes(x.id))
  const put = (ms: { contact_id: string; role: string }[]) => void act(people.putMembers(g.id, ms))
  const cur = members.map((m) => ({ contact_id: m.contact_id, role: m.role }))
  const savePurpose = () => { if (purpose !== null && purpose !== g.purpose) void act(people.patchTeam(g.id, { purpose: purpose.slice(0, 400) })) }
  return (
    <Page title={g.name} back={["#/contacts", "联系人"]} max={720} sub={<SubMeta><span>{TEAM_KIND[g.kind]}</span><span>{g.source_label}</span>{parent && <a href={"#/teams/" + parent.id} className="hover:text-foreground">上级：{parent.name}</a>}<span>{g.headcount} 人</span>{g.status === "dissolved" && <span>已解散</span>}</SubMeta>}
      actions={v && g.status === "active" ? <Button size="sm" variant="ghost" className="text-destructive" onClick={() => setDel(true)} data-del-team>解散</Button> : undefined}>
      <AlertDialog open={del} onOpenChange={setDel}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>解散 {g.name}？</AlertDialogTitle><AlertDialogDescription>联系人不受影响；事项里对这个团队的引用保留为“已解散的团队”。</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel><AlertDialogAction variant="destructive" data-del-team-confirm onClick={async () => { if (await act(people.dissolve(g.id), "已解散")) go("#/contacts") }}>解散</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
      <div className="flex flex-col gap-8">
        <AskSteve key={g.id} ctx={ctxOf("team", g.id)} placeholder={v ? `例如：把韩梅梅加到${g.name}；${g.name}每周三同步` : `例如：${g.name}的需求都交给需求官整理`} />
        <Section label="说明"><Input className="rounded-lg" aria-label="团队说明" maxLength={400} value={purpose ?? g.purpose} onChange={(e) => setPurpose(e.target.value)} onBlur={savePurpose} onKeyDown={(e) => { if (e.key === "Enter" && !e.nativeEvent.isComposing) (e.target as HTMLInputElement).blur() }} placeholder="这个团队做什么，Steve 会用它判断相关的事" data-team-purpose /></Section>
        <Section label={`成员 · ${members.length}`} right={v ? <AddMember have={members.map((m) => m.contact_id)} onPick={(cid) => put([...cur, { contact_id: cid, role: "member" }])} /> : undefined}>
          {members.length ? <Group>{members.map((m) => (
            <Row key={m.contact_id} icon={<ContactAvatar name={m.name} size={28} />} title={<span className="inline-flex items-center gap-1.5"><a href={"#/contacts/" + m.contact_id} className="hover:underline">{m.name}</a>{m.external && <Badge variant="outline" className="font-normal text-muted-foreground">外部</Badge>}</span>} data-member={m.name}>
              {v && m.source !== "sync" ? <><span><Select align="end" quiet testId={"role-" + m.name} value={m.role} onChange={(x) => put(cur.map((c) => (c.contact_id === m.contact_id ? { ...c, role: x } : c)))} options={Object.keys(ROLE).map((x) => ({ v: x, l: <span className="type-footnote">{ROLE[x]}</span> }))} /></span>
                <Button variant="ghost" size="icon-xs" aria-label={"移出 " + m.name} className="text-muted-foreground" onClick={() => put(cur.filter((c) => c.contact_id !== m.contact_id))} data-rm-member={m.name}><XIcon /></Button></>
                : <span className="type-footnote text-muted-foreground">{ROLE[m.role] ?? m.role}</span>}</Row>))}</Group> : <Empty title="还没有成员" />}
          {!v && <div className="mt-2 px-1 type-footnote text-muted-foreground">成员跟随通讯录同步，在这里不能改。</div>}
        </Section>
        {subs.length > 0 && <Section label="下级团队"><Group>{subs.map((x) => <TeamRow key={x.id} g={x} spaces={spaces.data} />)}</Group></Section>}
        {spaces.data && <Section label="绑定的群"><Group><Row title={spaces.data.find((x) => x.id === g.space_id)?.name ?? "没有绑定"} sub="团队的讨论默认在这个群里；Steve 在群里认得出这些成员属于这个团队">
          <Select align="end" testId="team-space" value={g.space_id ?? "none"} onChange={(x) => void act(people.patchTeam(g.id, { space_id: x === "none" ? null : x }))} options={[{ v: "none", l: "不绑定" }, ...spaces.data.map((x) => ({ v: x.id, l: x.name }))]} /></Row></Group></Section>}
        <Section label={`成员的事项 · ${items.data?.length ?? 0}`}>{!items.data ? <Loading /> : items.data.length ? <Group>{[...new Map(items.data.map((x) => [x.item_id, x])).values()].map((x) => { const i = st.items.find((y) => y.id === x.item_id); return <Row key={x.item_id} title={<span><ItemNo id={x.item_id} className="mr-2" />{i?.t ?? x.title ?? ""}</span>} sub={ITEM_ROLE[x.role] ?? x.role} onClick={() => go("#/items/" + x.item_id)}>{i && <Status s={i.st} />}</Row> })}</Group> : <Empty title="还没有相关事项" />}</Section>
        <MemSection party={g.id} mems={mems.data} label="Steve 记得的" />
        <div className="px-1 type-footnote text-muted-foreground">可见范围：{visLabel(g.visibility)}。团队也可以作为记忆和信息的可见范围，例如“只对 {g.name} 可见”。</div>
      </div>
    </Page>
  )
}

function AddMember({ have, onPick }: { have: string[]; onPick: (cid: string) => void }) {
  const [open, setOpen] = useState(false)
  const cs = useLoad(() => (open ? people.contacts({ limit: 500 }) : Promise.resolve(null)), [open])
  const xs = (cs.data ?? []).filter((c) => !have.includes(c.id))
  return (
    <DropdownMenu open={open} onOpenChange={setOpen}>
      <DropdownMenuTrigger render={<Button size="xs" variant="ghost" className="text-muted-foreground" data-add-member />}><PlusIcon />添加</DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="max-h-72 min-w-56">
        {!cs.data ? <div className="px-2 py-1.5 type-footnote text-muted-foreground">载入中…</div> : !xs.length ? <div className="px-2 py-1.5 type-footnote text-muted-foreground">没有可以添加的联系人</div>
          : xs.map((c) => <DropdownMenuItem key={c.id} onClick={() => onPick(c.id)} data-member-opt={c.name}><ContactAvatar name={c.name} size={18} />{c.is_me ? c.name + "（我）" : c.name}<span className="ml-auto type-footnote text-muted-foreground">{c.org?.name}</span></DropdownMenuItem>)}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

export function NewTeamDialog({ onClose }: { onClose: () => void }) {
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
        <DialogHeader><DialogTitle className="type-headline">新建虚拟团队</DialogTitle><DialogDescription className="type-callout">兴趣小组、跨部门协作、和客户一起的交付小组。</DialogDescription></DialogHeader>
        <FieldGroup className="gap-4">
          <Field><FieldLabel htmlFor="nt-name">名称</FieldLabel><Input id="nt-name" autoFocus className="rounded-lg" maxLength={40} value={n} onChange={(e) => setN(e.target.value)} placeholder="例如：Acme 交付小组" data-nt-name /></Field>
          <Field><FieldLabel htmlFor="nt-purpose">做什么</FieldLabel><Input id="nt-purpose" className="rounded-lg" maxLength={400} value={pur} onChange={(e) => setPur(e.target.value)} placeholder="Steve 会用它判断哪些事和这个团队有关" /></Field>
          <Field><FieldLabel>成员 · {pick.length}</FieldLabel>
            <div className="max-h-52 overflow-y-auto rounded-lg border" role="group" aria-label="选择成员">{!cs.data ? <Loading /> : xs.map((c) => (
              <label key={c.id} className="flex cursor-pointer items-center gap-2.5 px-3 py-2 hover:bg-accent" data-nt-pick={c.name}><Checkbox checked={pick.includes(c.id)} onCheckedChange={(v) => setPick(v ? [...pick, c.id] : pick.filter((x) => x !== c.id))} /><ContactAvatar name={c.name} size={20} /><span className="type-callout">{c.is_me ? c.name + "（我）" : c.name}</span><span className="ml-auto type-footnote text-muted-foreground">{c.org?.name}</span></label>))}</div></Field>
        </FieldGroup>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={onClose}>取消</Button><Button disabled={!n.trim() || pick.length < 2 || busy} data-testid="nt-go" onClick={() => void create()}>建立</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
