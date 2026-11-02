/* #/orgs/:id — one organization (prototype people.tsx OrgView) */
import { useState } from "react"
import { ChevronRightIcon, PlusIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { st } from "@/store"
import { go, Page, Group, Row, Empty, ItemNo, Status } from "@/ui"
import { AskSteve } from "@/config-ui"
import { people } from "@/api/people"
import { ContactAvatar, ITEM_ROLE, Loading, ORG_KIND, PeopleGate, Section, useLoad , SubMeta } from "@/people/common"
import { AttrRow, MemSection, NewAttr, ctxOf } from "@/people/contact"

export function OrgView({ id }: { id: string }) { return <PeopleGate>{() => <OrgPage id={id} />}</PeopleGate> }
function OrgPage({ id }: { id: string }) {
  const { data: o, err } = useLoad(() => people.org(id), [id])
  const items = useLoad(() => people.items(id), [id], ["party", "item"])
  const mems = useLoad(() => people.memories(id), [id])
  const [newAttr, setNewAttr] = useState(false)
  if (err) return <Page title="组织" back={["#/contacts", "联系人"]}><Empty title={err.status === 404 ? "组织不存在" : "组织载入失败"} sub={err.status === 404 ? undefined : err.message} /></Page>
  if (!o) return <Page title="组织" back={["#/contacts", "联系人"]}><Loading /></Page>
  const ps = o.people ?? []
  return (
    <Page title={o.name} back={["#/contacts", "联系人"]} max={720} sub={<SubMeta>{o.kind && <span>{ORG_KIND[o.kind] ?? o.kind}</span>}<span>{o.domains.join("、") || "无域名"}</span>{o.aliases.length > 0 && <span>也叫 {o.aliases.join("、")}</span>}</SubMeta>}>
      <div className="flex flex-col gap-8">
        {o.note && <p className="-mt-2 whitespace-pre-wrap type-body text-muted-foreground">{o.note}</p>}
        <AskSteve key={o.id} ctx={ctxOf("org", o.id)} placeholder={`例如：${o.name} 的域名还有 …；${o.name} 是我们的客户`} />
        <Section label={`联系人 · ${ps.length}`}>{ps.length ? <Group>{ps.map((c) => <Row key={c.id} icon={<ContactAvatar name={c.name} size={28} />} title={c.name} sub={c.title ?? undefined} onClick={() => go("#/contacts/" + c.id)} data-org-person={c.name}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Group> : <Empty title="还没有联系人" />}</Section>
        <Section label="资料" right={<Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setNewAttr(true)} data-add-attr><PlusIcon />添加</Button>}>
          <Group>
            <Row title="域名" sub="这些域名的邮箱默认归到这个组织；邮箱前缀规则只在同组织域名之间生效"><span className="type-callout">{o.domains.join("、") || "—"}</span></Row>
            {o.attrs.map((a) => <AttrRow key={a.id} a={a} />)}
            {newAttr && <NewAttr party={o.id} init={{ label: "" }} onDone={() => setNewAttr(false)} />}
          </Group>
        </Section>
        <Section label={`相关事项 · ${items.data?.length ?? 0}`}>{!items.data ? <Loading /> : items.data.length ? <Group>{items.data.map((x) => { const i = st.items.find((y) => y.id === x.item_id); return <Row key={x.item_id + x.role} title={<span><ItemNo id={x.item_id} className="mr-2" />{i?.t ?? x.title ?? ""}</span>} sub={ITEM_ROLE[x.role] ?? x.role} onClick={() => go("#/items/" + x.item_id)}>{i && <Status s={i.st} />}</Row> })}</Group> : <Empty title="还没有相关事项" />}</Section>
        <MemSection party={o.id} mems={mems.data} label="Steve 记得的" />
      </div>
    </Page>
  )
}
