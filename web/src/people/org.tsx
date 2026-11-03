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
import { ContactAvatar, ITEM_ROLE, Loading, ORG_KIND, PeopleGate, Section, act, useLoad , SubMeta } from "@/people/common"
import { AttrRow, MemSection, NewAttr, ctxOf } from "@/people/contact"

export function OrgView({ id }: { id: string }) { return <PeopleGate>{() => <OrgPage id={id} />}</PeopleGate> }
function OrgPage({ id }: { id: string }) {
  const { data: o, err, reload } = useLoad(() => people.org(id), [id])
  const items = useLoad(() => people.items(id), [id], ["party", "item"])
  const mems = useLoad(() => people.memories(id), [id])
  const [newAttr, setNewAttr] = useState(false), [edit, setEdit] = useState(false)
  if (err) return <Page title="组织" back={["#/contacts", "联系人"]}><Empty title={err.status === 404 ? "组织不存在" : "组织载入失败"} sub={err.status === 404 ? undefined : err.message} /></Page>
  if (!o) return <Page title="组织" back={["#/contacts", "联系人"]}><Loading /></Page>
  const ps = o.people ?? []
  return (
    <Page actions={<Button size="sm" variant="outline" onClick={() => setEdit(true)}>编辑组织</Button>} title={o.name} back={["#/contacts", "联系人"]} max={720} sub={<SubMeta>{o.kind && <span>{ORG_KIND[o.kind] ?? o.kind}</span>}<span>{o.domains.join("、") || "无域名"}</span>{o.aliases.length > 0 && <span>也叫 {o.aliases.join("、")}</span>}</SubMeta>}>
      {edit && <EditOrg o={o} onClose={() => setEdit(false)} onSaved={() => { setEdit(false); void reload() }} />}
      <div className="flex flex-col gap-8">
        {(o.name_state === "placeholder" || o.name_state === "workspace") && <div className="rounded-lg border p-3 text-sm"><p>{o.name_reason === "external_unavailable" ? "飞书不向应用提供外部组织名称，请手动命名。" : o.name_reason === "permission" ? `尚无读取组织名称的权限${o.name_scope ? `（${o.name_scope}）` : ""}，也可以手动命名。` : "组织名称尚未补全，可以手动命名。"}</p><Button size="sm" variant="outline" className="mt-2" onClick={() => setEdit(true)}>命名组织</Button></div>}
        {o.tenant && <div className="break-all text-xs text-muted-foreground">飞书组织标识：{o.tenant.key}</div>}
        {o.note && <p className="-mt-2 whitespace-pre-wrap type-body text-muted-foreground">{o.note}</p>}
        <AskSteve key={o.id} ctx={ctxOf("org", o.id)} placeholder={`例如：${o.name} 的域名还有 …；${o.name} 是我们的客户`} />
        <Section label={`联系人 · ${ps.length}`}>{ps.length ? <Group>{ps.map((c) => <Row key={c.id} icon={<ContactAvatar name={c.name} size={28} />} title={c.name} sub={c.title ?? undefined} onClick={() => go("#/contacts/" + c.id)} data-org-person={c.name}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Group> : <Empty title="还没有联系人" />}</Section>
        <Section label="资料" right={<Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setNewAttr(true)} data-add-attr><PlusIcon />添加</Button>}>
          <Group>
            <Row title="域名" sub="这些域名的邮箱默认归到这个组织；邮箱前缀规则只在同组织域名之间生效"><span className="type-callout">{o.domains.join("、") || "—"}</span></Row>
            {o.attrs.map((a) => <AttrRow key={a.id} a={a} onChanged={() => void reload()} />)}
            {newAttr && <NewAttr party={o.id} init={{ label: "" }} onDone={() => { setNewAttr(false); void reload() }} />}
          </Group>
        </Section>
        <Section label={`相关事项 · ${items.data?.length ?? 0}`}>{!items.data ? <Loading /> : items.data.length ? <Group>{items.data.map((x) => { const i = st.items.find((y) => y.id === x.item_id); return <Row key={x.item_id + x.role} title={<span><ItemNo id={x.item_id} className="mr-2" />{i?.t ?? x.title ?? ""}</span>} sub={ITEM_ROLE[x.role] ?? x.role} onClick={() => go("#/items/" + x.item_id)}>{i && <Status s={i.st} />}</Row> })}</Group> : <Empty title="还没有相关事项" />}</Section>
        <MemSection party={o.id} mems={mems.data} label="Steve 记得的" />
      </div>
    </Page>
  )
}

function EditOrg({ o, onClose, onSaved }: { o: Org; onClose: () => void; onSaved: () => void }) {
  const [name, setName] = useState(o.name), [aliases, setAliases] = useState(o.aliases.join("、")), [domains, setDomains] = useState(o.domains.join("、")), [kind, setKind] = useState(o.kind ?? "other"), [note, setNote] = useState(o.note), [busy, setBusy] = useState(false)
  const split = (s: string) => [...new Set(s.split(/[、,，\s]+/).filter(Boolean))]
  const save = async () => {
    setBusy(true)
    const r = await act(people.patchOrg(o.id, { name: name.trim(), aliases: split(aliases), domains: split(domains), note, ...(o.kind === "self" ? {} : { kind }) }), "组织资料已保存")
    setBusy(false); if (r) onSaved()
  }
  return <Dialog open onOpenChange={(v) => !v && !busy && onClose()}><DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-[460px]">
    <DialogHeader><DialogTitle>编辑组织</DialogTitle><DialogDescription>填写可辨认的组织名称；域名和备注可以留空。</DialogDescription></DialogHeader>
    <label className="space-y-1 text-sm">组织名称<Input aria-label="组织名称" value={name} maxLength={80} onChange={(e) => setName(e.target.value)} /></label>
    <label className="space-y-1 text-sm">别名<Input aria-label="组织别名" value={aliases} placeholder="用顿号分开" onChange={(e) => setAliases(e.target.value)} /></label>
    <label className="space-y-1 text-sm">域名<Input aria-label="组织域名" value={domains} placeholder="例如 example.com" onChange={(e) => setDomains(e.target.value)} /></label>
    <label className="space-y-1 text-sm">类型<select aria-label="组织类型" className="block h-9 w-full rounded-md border bg-background px-2" disabled={o.kind === "self" || busy} value={kind} onChange={(e) => setKind(e.target.value)}>{Object.entries(ORG_KIND).filter(([k]) => k !== "self" || o.kind === "self").map(([k, label]) => <option key={k} value={k}>{label}</option>)}</select></label>
    <label className="space-y-1 text-sm">备注<Textarea aria-label="组织备注" value={note} maxLength={2000} onChange={(e) => setNote(e.target.value)} /></label>
    <DialogFooter><Button variant="outline" disabled={busy} onClick={onClose}>取消</Button><Button disabled={busy || !name.trim()} onClick={() => void save()}>保存</Button></DialogFooter>
  </DialogContent></Dialog>
}
