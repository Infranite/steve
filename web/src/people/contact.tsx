/* #/contacts/:id and #/contacts/me — one contact (prototype people.tsx ContactView) */
import { useState } from "react"
import { cn } from "cn"
import { CheckIcon, ChevronRightIcon, EllipsisIcon, ListTodoIcon, MailIcon, MessageSquareIcon, CalendarIcon, PlusIcon, StarIcon, XIcon, ZapIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Field, FieldGroup, FieldLabel } from "@/components/ui/field"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { st, tm } from "@/store"
import { go, Page, Group, Row, Meta, Empty, Tags, ItemNo, Status } from "@/ui"
import { AskSteve, type Ctx } from "@/config-ui"
import { nameOf, people, type Attr, type Contact, type Level, type Mem } from "@/api/people"
import { ContactAvatar, ITEM_ROLE, KIND, LEVEL, LevelSel, Loading, MEM_KIND, PeopleGate, REL, ROLE, Section, TEAM_KIND, TOUCH, act, day, relLabel, useLoad, visLabel , SubMeta } from "@/people/common"
import { orgLine } from "@/people/contacts"
import { MergeDialog } from "@/people/pending"
import { HoldingsSection } from "@/people/holdings"

const TOUCH_I: Record<string, any> = { mail: MailIcon, message: MessageSquareIcon, meeting: CalendarIcon, item: ListTodoIcon, event: ZapIcon }
/** “我”的页先列这几项，没填的也占一行 */
const ME_KEYS: [string, string][] = [["call", "偏好的称呼"], ["tz", "时区"], ["lang", "语言"]]
/* AskSteve's Ctx names the kinds it knew about; the Hub takes contact / org / team too (contacts.md §5) */
export const ctxOf = (kind: "contact" | "org" | "team" | "global", id?: string) => ({ kind, id }) as unknown as Ctx
const first = (n: string) => n.split(" ")[0]
const keyFor = (label: string) => { let h = 2166136261; for (const ch of label) { h ^= ch.codePointAt(0)!; h = Math.imul(h, 16777619) } return "x_" + (h >>> 0).toString(36) }

export function ContactView({ id }: { id: string }) { return <PeopleGate>{() => <ContactPage id={id} />}</PeopleGate> }
function ContactPage({ id }: { id: string }) {
  const { data: c, err } = useLoad(() => people.contact(id), [id])
  if (err) return <Page title="联系人" back={["#/contacts", "联系人"]}><Empty title={err.status === 404 ? "联系人不存在" : "联系人载入失败"} sub={err.status === 404 ? "可能已经被删除或合并。" : err.message} /></Page>
  if (!c) return <Page title="联系人" back={["#/contacts", "联系人"]}><Loading /></Page>
  if (c.status === "merged" && c.merged_into) return <Page title={c.name} back={["#/contacts", "联系人"]}><Empty title="已经合并" sub="这个联系人已经合并到另一个联系人。" /><div className="flex justify-center"><Button variant="outline" onClick={() => go("#/contacts/" + c.merged_into)}>去看合并后的联系人</Button></div></Page>
  return <ContactBody c={c} />
}

function ContactBody({ c }: { c: Contact }) {
  const [del, setDel] = useState(false), [edit, setEdit] = useState(false), [merge, setMerge] = useState(false)
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
  const attrs = c.is_me ? [...ME_KEYS.flatMap(([k]) => c.attrs.filter((a) => a.key === k)), ...c.attrs.filter((a) => !ME_KEYS.some(([k]) => k === a.key))] : c.attrs
  const missing = c.is_me ? ME_KEYS.filter(([k]) => !c.attrs.some((a) => a.key === k)) : []
  const setRels = (r: string) => void act(people.patchContact(c.id, { rels: c.rels.includes(r) ? c.rels.filter((x) => x !== r) : [...c.rels, r] }))
  return (
    <Page title={nameOf(c)} back={["#/contacts", "联系人"]} max={760}
      sub={<Meta>{orgLine(c) && (orgId ? <a href={"#/orgs/" + orgId} className="hover:text-foreground">{orgLine(c)}</a> : <span>{orgLine(c)}</span>)}{rels.length > 0 && <span>{rels.map(relLabel).join("、")}</span>}
        {c.kind && c.kind !== "real" && <span>{KIND[c.kind]}</span>}{c.aliases.length > 0 && <span>也叫 {c.aliases.join("、")}</span>}<span>{visLabel(c.visibility)}</span></Meta>}
      actions={<>
        <Button variant="ghost" size="icon-sm" aria-label={c.important ? "取消重要" : "标为重要"} aria-pressed={c.important} className="text-muted-foreground" onClick={() => void act(people.patchContact(c.id, { important: !c.important }))} data-star-contact><StarIcon className={cn(c.important && "fill-current text-foreground")} /></Button>
        <DropdownMenu>
          <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label="更多" className="text-muted-foreground" data-contact-more />}><EllipsisIcon /></DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="min-w-44">
            <DropdownMenuItem onClick={() => setEdit(true)} data-edit-contact>编辑名字和备注</DropdownMenuItem>
            {!c.is_me && <DropdownMenuItem onClick={() => setMerge(true)} data-merge-contact>合并到另一个联系人…</DropdownMenuItem>}
            {!c.is_me && <><DropdownMenuSeparator /><DropdownMenuItem variant="destructive" onClick={() => setDel(true)} data-del-contact>删除</DropdownMenuItem></>}
          </DropdownMenuContent>
        </DropdownMenu></>}>
      <AlertDialog open={del} onOpenChange={setDel}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>删除 {c.name}？</AlertDialogTitle><AlertDialogDescription>{accts.data ? `${accts.data.length} 个账号、` : ""}{c.attrs.length} 个属性、{mems.data?.length ?? 0} 条记忆一起删除；事项和消息里的引用改为“已删除的联系人”。</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel><AlertDialogAction variant="destructive" data-del-confirm onClick={async () => { if (await act(people.deleteContact(c.id), "已删除")) go("#/contacts") }}>删除</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
      {edit && <EditDialog c={c} onClose={() => setEdit(false)} />}
      {merge && <MergeDialog from={c} onClose={() => setMerge(false)} />}
      <div className="flex flex-col gap-8">
        {unconfirmed && <div className="flex flex-wrap items-center gap-3 rounded-xl border px-4 py-3" data-unconfirmed-banner>
          <div className="min-w-0 flex-1"><div className="type-headline">未确认的联系人</div><div className="mt-0.5 type-callout text-muted-foreground">Steve 认不出这个账号的主人。照常处理消息，但不会被当作熟人对待。</div></div>
          <Button size="sm" variant="outline" onClick={() => setMerge(true)}>合并到…</Button>
          <Button size="sm" onClick={() => void act(people.confirm(c.id), "已确认为新联系人")} data-confirm-contact>确认为新联系人</Button></div>}
        <div className="-mt-2 flex items-center gap-4"><ContactAvatar name={c.name} unconfirmed={unconfirmed} size={56} /><div className="min-w-0 flex-1"><div className="type-body whitespace-pre-wrap">{c.note || (c.is_me ? "这是你自己" : "还没有备注")}</div><div className="mt-1 flex flex-wrap items-center gap-2"><Tags ids={c.tags} />
          {!c.is_me && <DropdownMenu><DropdownMenuTrigger render={<button type="button" className="type-footnote text-muted-foreground hover:text-foreground" data-rel-menu />}>{rels.length ? "改关系" : "+ 关系"}</DropdownMenuTrigger>
            <DropdownMenuContent align="start" className="min-w-36">{Object.keys(REL).filter((x) => x !== "me").map((x) => <DropdownMenuItem key={x} onClick={() => setRels(x)} data-rel-opt={x}>{c.rels.includes(x) ? <CheckIcon /> : <span className="size-4" />}{REL[x]}</DropdownMenuItem>)}</DropdownMenuContent></DropdownMenu>}</div></div></div>
        <AskSteve key={c.id} ctx={ctxOf("contact", c.id)} placeholder={c.is_me ? "告诉 Steve 关于你的事，例如：叫我小李；我在上海，说中文；邮件先给结论" : `告诉 Steve 关于 ${first(c.name)} 的事，例如：换到 Globex 做产品负责人了；电话是 …；以后叫她 Sarah`} />
        <Section label="资料" right={<Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setNewAttr({ label: "" })} data-add-attr><PlusIcon />添加</Button>}>
          <Group>
            {!c.is_me && <Row title="组织与职位" sub={past.length ? "之前：" + past.map((p) => `${p.org_name}${p.title ? " · " + p.title : ""}（到 ${p.to}）`).join("；") : undefined}><span className="type-callout">{now.map((j) => [j.org_name, j.title].filter(Boolean).join(" · ")).join("；") || "—"}</span></Row>}
            {attrs.map((a) => <AttrRow key={a.id} a={a} />)}
            {missing.map(([k, l]) => <Row key={k} title={l} sub="还没填"><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setNewAttr({ key: k, label: l })} data-fill-attr={k}><PlusIcon />填写</Button></Row>)}
            {newAttr && <NewAttr party={c.id} init={newAttr} onDone={() => setNewAttr(null)} />}
          </Group>
          <div className="mt-2 px-1 type-footnote text-muted-foreground">每项资料都有披露级别：在群里回复时，超出听众范围的资料不会被说出来。属性可以自己加，也可以在设置里统一定义。</div>
        </Section>
        <HoldingsSection person={c.id} name={first(c.name)} isMe={c.is_me} isBot={c.kind === "bot"} />
        <Section label={`所在团队 · ${c.teams.length}`}>{c.teams.length ? <Group>{c.teams.map((g) => <Row key={g.id} icon={<ContactAvatar name={g.name} type="team" size={28} />} title={g.name} sub={`${TEAM_KIND[g.kind] ?? g.kind} · ${ROLE[g.role] ?? g.role}`} onClick={() => go("#/teams/" + g.id)} data-contact-team={g.name}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Group> : <Empty title="不在任何团队里" />}</Section>
        <Section label={`相关事项 · ${items.data?.length ?? 0}`}>
          {!items.data ? <Loading /> : items.data.length ? <Group>{items.data.map((x) => { const i = st.items.find((y) => y.id === x.item_id); return (
            <Row key={x.item_id + x.role} title={<span><ItemNo id={x.item_id} className="mr-2" />{i?.t ?? x.title ?? ""}</span>} sub={<SubMeta><span>{ITEM_ROLE[x.role] ?? x.role}</span>{x.confidence < 1 && <span>Steve 推断</span>}</SubMeta>} onClick={() => go("#/items/" + x.item_id)} data-party-item={x.item_id}>{i && <Status s={i.st} />}</Row>) })}</Group> : <Empty title="还没有相关事项" />}
        </Section>
        <Section label="往来">
          {!tl.data ? <Loading /> : tl.data.length ? <div className="flex flex-col" data-touches>{tl.data.map((t, n) => { const I = TOUCH_I[t.kind] ?? ZapIcon; return (
            <button key={n} type="button" disabled={!t.href} onClick={() => t.href && go(t.href)} className="flex items-start gap-3 rounded-lg px-2 py-2 text-left transition-colors enabled:hover:bg-accent">
              <I className="mt-0.5 size-4 shrink-0 text-muted-foreground" strokeWidth={1.5} aria-hidden /><span className="min-w-0 flex-1 type-callout">{t.text}</span><span className="shrink-0 type-footnote text-muted-foreground">{TOUCH[t.kind] ?? t.kind} · {tm(t.ts)}</span></button>) })}</div> : <Empty title="还没有往来记录" />}
        </Section>
        <MemSection party={c.id} mems={mems.data} label={c.is_me ? "Steve 记得的关于你" : "Steve 记得的"} />
        {c.merges.length > 0 && <Section label="合并记录"><Group>{c.merges.map((m) => (
          <Row key={m.id} title={`合并了 ${m.from_name}`} sub={`${day(m.created)}${m.splittable ? " · 可以拆回" : ""}`} data-merge-rec={m.id}>
            {m.splittable && <Button size="xs" variant="outline" onClick={() => void act(people.split(m.id), "已拆回")} data-split={m.id}>拆回</Button>}</Row>))}</Group></Section>}
        {mates.length > 0 && <Section label="同组织的联系人"><Group>{mates.map((x) => <Row key={x.id} icon={<ContactAvatar name={x.name} size={28} />} title={x.name} sub={x.title ?? undefined} onClick={() => go("#/contacts/" + x.id)}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Group></Section>}
      </div>
    </Page>
  )
}

export function AttrRow({ a, readOnly }: { a: Attr; readOnly?: boolean }) {
  return (
    <Row title={<span className="inline-flex items-center gap-2">{a.label}{a.status === "candidate" && <Badge variant="outline" className="font-normal text-attention">待确认</Badge>}{a.status === "stale" && <Badge variant="outline" className="font-normal text-muted-foreground">已过时</Badge>}</span>} sub={`来源：${a.source_label}`} data-attr={a.label}>
      <span className="max-w-[34vw] truncate type-callout sm:max-w-[260px]">{a.value}</span>
      {readOnly ? <span className="type-footnote text-muted-foreground">{LEVEL[a.level]}</span> : <>
        <LevelSel v={a.level} on={(l) => void act(people.patchAttr(a.id, { level: l }))} testId={"level-" + a.key} />
        <Button variant="ghost" size="icon-xs" aria-label={"删除 " + a.label} className="text-muted-foreground" onClick={() => void act(people.deleteAttr(a.id))} data-del-attr={a.label}><XIcon /></Button></>}
    </Row>
  )
}

export function NewAttr({ party, init, onDone }: { party: string; init: { key?: string; label: string }; onDone: () => void }) {
  const [label, setLabel] = useState(init.label), [v, setV] = useState(""), [busy, setBusy] = useState(false)
  const defs = useLoad(() => people.attrDefs().catch(() => []), [])
  const save = async () => {
    const l = label.trim(), d = defs.data?.find((x) => x.label === l || x.key === l)
    setBusy(true)
    const r = await act(people.addAttr(party, { key: init.key ?? d?.key ?? keyFor(l), label: l.slice(0, 20), value: v.trim() }))
    setBusy(false); if (r) onDone()
  }
  return (
    <div className="flex flex-wrap items-center gap-2 px-2 py-2.5" role="group" aria-label="添加资料">
      <Input className="h-8 w-32 rounded-lg" aria-label="资料名称" placeholder="名称，如 生日" list="attr-defs" value={label} disabled={!!init.key} onChange={(e) => setLabel(e.target.value)} data-attr-k />
      <datalist id="attr-defs">{(defs.data ?? []).map((d) => <option key={d.id} value={d.label} />)}</datalist>
      <Input autoFocus className="h-8 min-w-40 flex-1 rounded-lg" aria-label="值" placeholder="值" maxLength={500} value={v} onChange={(e) => setV(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && label.trim() && v.trim() && !e.nativeEvent.isComposing) void save() }} data-attr-v />
      <Button size="sm" disabled={!label.trim() || !v.trim() || busy} onClick={() => void save()} data-attr-save>保存</Button><Button size="sm" variant="ghost" onClick={onDone}>取消</Button>
    </div>
  )
}

export function MemSection({ party, mems, label, editable = true }: { party: string; mems: Mem[] | null; label: string; editable?: boolean }) {
  const [adding, setAdding] = useState(false), [t, setT] = useState("")
  const add = async () => { if (await act(people.addMemory(party, { text: t.trim() }))) { setT(""); setAdding(false) } }
  return (
    <Section label={`${label} · ${mems?.length ?? 0}`} right={editable ? <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setAdding(true)} data-add-mem><PlusIcon />添加</Button> : undefined}>
      {adding && <div className="mb-2 flex items-center gap-2"><Input autoFocus className="h-8 flex-1 rounded-lg" aria-label="新的记忆" placeholder="例如：喜欢简短的邮件，先给结论" maxLength={2000} value={t} onChange={(e) => setT(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && t.trim() && !e.nativeEvent.isComposing) void add() }} data-mem-input />
        <Button size="sm" disabled={!t.trim()} onClick={() => void add()} data-mem-save>记下</Button><Button size="sm" variant="ghost" onClick={() => setAdding(false)}>取消</Button></div>}
      {!mems ? <Loading /> : mems.length ? <Group>{mems.map((m) => (
        <Row key={m.id} title={<span className="inline-flex items-center gap-2">{m.text}{m.status === "candidate" && <Badge variant="outline" className="font-normal text-attention">待确认</Badge>}{m.status === "stale" && <Badge variant="outline" className="font-normal text-muted-foreground">已过时</Badge>}</span>} sub={`${MEM_KIND[m.kind] ?? m.kind} · 来源：${m.source_label}`} data-memo={m.text}>
          {m.status === "candidate" && <><Button size="xs" variant="outline" onClick={() => void act(people.patchMem(m.id, { status: "confirmed" }))} data-memo-ok>确认</Button><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => void act(people.deleteMem(m.id))}>不对</Button></>}
          <LevelSel v={m.level} on={(l: Level) => void act(people.patchMem(m.id, { level: l }))} />
          {m.status !== "candidate" && <Button variant="ghost" size="icon-xs" aria-label="删除这条记忆" className="text-muted-foreground" onClick={() => void act(people.deleteMem(m.id))}><XIcon /></Button>}</Row>))}</Group>
        : <Empty title="还没有记忆" sub="往来和事项里提到的事实、偏好会记在这里。" />}
    </Section>
  )
}

function EditDialog({ c, onClose }: { c: Contact; onClose: () => void }) {
  const [n, setN] = useState(c.name), [al, setAl] = useState(c.aliases.join("、")), [note, setNote] = useState(c.note), [busy, setBusy] = useState(false)
  const save = async () => {
    setBusy(true)
    const aliases = al.split(/[、,，\s]+/).map((x) => x.trim()).filter(Boolean).slice(0, 10)
    const r = await act(people.patchContact(c.id, { name: n.trim(), aliases, note }))
    setBusy(false); if (r) onClose()
  }
  return (
    <Dialog open onOpenChange={(x) => !x && onClose()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[460px]">
        <DialogHeader><DialogTitle className="type-headline">编辑 {c.name}</DialogTitle><DialogDescription className="type-callout">名字和别名用来认出 @ 和对话里提到的人。</DialogDescription></DialogHeader>
        <FieldGroup className="gap-4">
          <Field><FieldLabel htmlFor="ec-name">名字</FieldLabel><Input id="ec-name" className="rounded-lg" maxLength={80} value={n} onChange={(e) => setN(e.target.value)} data-ec-name /></Field>
          <Field><FieldLabel htmlFor="ec-alias">别名（用顿号分开）</FieldLabel><Input id="ec-alias" className="rounded-lg" value={al} onChange={(e) => setAl(e.target.value)} placeholder="例如：Sarah、陈总" data-ec-alias /></Field>
          <Field><FieldLabel htmlFor="ec-note">备注</FieldLabel><Textarea id="ec-note" className="min-h-20 rounded-lg" maxLength={2000} value={note} onChange={(e) => setNote(e.target.value)} data-ec-note /></Field>
        </FieldGroup>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={onClose}>取消</Button><Button disabled={!n.trim() || busy} onClick={() => void save()} data-ec-save>保存</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
