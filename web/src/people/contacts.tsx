/* #/contacts — 联系人、团队、组织、待确认 (prototype people.tsx ContactsView, NewContactDialog) */
import { useEffect, useState } from "react"
import { ChevronRightIcon, PlusIcon, StarIcon, UserRoundIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Input } from "@/components/ui/input"
import { Field, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { go, Page, Group, Row, Segmented, SectionLabel, Empty, Chips, Tags } from "@/ui"
import { Select } from "@/views"
import { AskSteve } from "@/config-ui"
import { myContactId, nameOf, people, type ContactBrief } from "@/api/people"
import { ContactAvatar, Loading, ORG_KIND, PeopleGate, REL, act, relLabel, useLoad , SubMeta } from "@/people/common"
import { DirectoryNotes, TeamRow, TeamsTab } from "@/people/teams"
import { PendingContacts } from "@/people/pending"
import { UnownedSection } from "@/people/holdings"

type Tab = "联系人" | "团队" | "组织" | "待确认"
/* the tab, filter and search survive leaving the page and coming back, as in the prototype */
let ctab: Tab = "联系人", crel = "全部", cq = ""
const FILTERS: Record<string, { rel?: string; kind?: string }> = { 全部: {}, 同事: { rel: "colleague" }, 客户: { rel: "customer" }, 合作方: { rel: "partner" }, 虚拟账号: { kind: "virtual" } }
/** “公司 · 部门路径 · 职位” */
export const orgLine = (c: Pick<ContactBrief, "org">) => [c.org?.name, c.org?.dept, c.org?.title].filter(Boolean).join(" · ")

export function ContactsView() { return <PeopleGate>{() => <Contacts />}</PeopleGate> }
function Contacts() {
  const [tab, setTab] = useState<Tab>(ctab), [rel, setRel] = useState(crel), [q, setQ] = useState(cq), [dq, setDq] = useState(cq), [newOpen, setNewOpen] = useState(false)
  useEffect(() => { const t = setTimeout(() => setDq(q.trim()), 200); return () => clearTimeout(t) }, [q])
  const pend = useLoad(() => people.pending(), [])
  const n = pend.data ? pend.data.proposals.length + pend.data.unconfirmed.length : 0
  return (
    <Page title="联系人" sub="和你打交道的人和组织，以及他们在各个系统里的账号" max={760}
      actions={<><Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => go("#/contacts/me")} data-my-contact><UserRoundIcon />我</Button>
        <Button size="sm" onClick={() => setNewOpen(true)} data-new-contact><PlusIcon />新建联系人</Button></>}
      toolbar={<Segmented value={tab} onChange={(v) => { ctab = v; setTab(v) }} options={[["联系人", "联系人"], ["团队", "团队"], ["组织", "组织"],
        ["待确认", <span key="p" className="inline-flex items-center gap-1">待确认{n > 0 && <span className="rounded-full bg-attention px-1.5 text-[10px] font-semibold text-white" aria-label={`${n} 个待确认`} data-pending-count>{n}</span>}</span>, "待确认"]] as const} />}>
      {newOpen && <NewContactDialog onClose={() => setNewOpen(false)} />}
      <AskSteve className="mb-6" ctx={{ kind: "global" }} placeholder="直接说，例如：Sarah 换到 Globex 做产品负责人了；Tom 的电话是 +1 415 555 0199" />
      {tab === "联系人" && <>
        <div className="mb-5 flex flex-wrap items-center gap-2"><Chips value={rel} onChange={(v) => { crel = v; setRel(v) }} options={Object.keys(FILTERS)} /><span className="flex-1" />
          <Input className="h-7 w-48 rounded-lg text-[13px]" aria-label="搜索联系人" placeholder="搜索名字、组织、邮箱" value={q} onChange={(e) => { cq = e.target.value; setQ(e.target.value) }} data-contact-search /></div>
        <ContactList q={dq} f={FILTERS[rel] ?? {}} />
        {!dq && rel === "全部" && <UnownedSection />}
      </>}
      {tab === "团队" && <TeamsTab />}
      {tab === "组织" && <OrgList />}
      {tab === "待确认" && <PendingContacts />}
    </Page>
  )
}

function ContactList({ q, f }: { q: string; f: { rel?: string; kind?: string } }) {
  const { data, err } = useLoad(() => people.contacts({ q, ...f }), [q, f.rel, f.kind])
  if (err) return <Empty title="联系人载入失败" sub={err.message} />
  if (!data) return <Loading />
  const xs = data.filter((c) => !c.is_me)
  if (!xs.length) return <Empty title={q || f.rel || f.kind ? "没有匹配的联系人" : "还没有联系人"} sub={q || f.rel || f.kind ? undefined : "新建一个，或者让 Steve 从邮件和消息里认出来。"} />
  return (
    <Group>{xs.map((c) => (
      <Row key={c.id} icon={<ContactAvatar name={nameOf(c)} size={36} />} onClick={() => go("#/contacts/" + c.id)} data-contact-row={c.name}
        title={<span className="inline-flex items-center gap-1.5">{nameOf(c)}{c.important && <StarIcon className="size-3.5 fill-current text-tertiary" aria-label="重要" />}{c.kind === "virtual" && <Badge variant="outline" className="font-normal text-muted-foreground">虚拟账号</Badge>}{c.kind === "bot" && <Badge variant="outline" className="font-normal text-muted-foreground">机器人</Badge>}</span>}
        sub={<SubMeta>{orgLine(c) && <span>{orgLine(c)}</span>}{c.rels.filter((r) => r !== "me").length > 0 && <span>{c.rels.filter((r) => r !== "me").map(relLabel).join("、")}</span>}<span>{c.accounts} 个账号</span>{c.items > 0 && <span>{c.items} 件事项</span>}</SubMeta>}>
        <Tags ids={c.tags} max={2} /><ChevronRightIcon className="size-4 text-tertiary" /></Row>))}</Group>
  )
}

function OrgList() {
  const { data, err } = useLoad(() => people.orgs(), [])
  const teams = useLoad(() => people.teams().catch(() => null), [])
  if (err) return <Empty title="组织载入失败" sub={err.message} />
  if (!data) return <Loading />
  if (!data.length) return <Empty title="还没有组织" sub="新建联系人时选组织，或者对 Steve 说“Sarah 在 Acme 做产品经理”。" />
  const chart = teams.data?.chart.filter((g) => g.status === "active") ?? [], own = data.find((o) => o.kind === "self")
  return (
    <div className="flex flex-col gap-8">
      <Group>{data.map((o) => (
        <Row key={o.id} icon={<ContactAvatar name={o.name} type="org" size={36} />} onClick={() => go("#/orgs/" + o.id)} data-org-row={o.name} title={o.name}
          sub={<SubMeta>{o.kind && <span>{ORG_KIND[o.kind] ?? o.kind}</span>}<span>{o.domains.join("、") || "无域名"}</span></SubMeta>}><ChevronRightIcon className="size-4 text-tertiary" /></Row>))}</Group>
      {teams.data && (chart.length > 0 || (teams.data.directory?.length ?? 0) > 0) && <section data-dept-tree>
        <SectionLabel>{own ? `${own.name} 的部门` : "部门"} · {chart.length}</SectionLabel>
        <DirectoryNotes ds={teams.data.directory} />
        {chart.length ? <Group>{chart.map((g) => <TeamRow key={g.id} g={g} indent={g.depth ?? 0} />)}</Group> : <Empty title="还没有同步到部门" sub="开通通讯录权限、设好可见范围后，部门树会自动同步到这里。" />}
      </section>}
    </div>
  )
}

export function NewContactDialog({ onClose }: { onClose: () => void }) {
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
        <DialogHeader><DialogTitle className="type-headline">新建联系人</DialogTitle><DialogDescription className="type-callout">也可以不填，Steve 从邮件和消息里认出来的人会自动建好，放在“待确认”。</DialogDescription></DialogHeader>
        <FieldGroup className="gap-4">
          <Field><FieldLabel htmlFor="nc-name">名字</FieldLabel><Input id="nc-name" autoFocus className="rounded-lg" maxLength={80} value={n} onChange={(e) => setN(e.target.value)} data-nc-name /></Field>
          <Field orientation="horizontal" className="justify-between"><FieldLabel>和你的关系</FieldLabel><Select testId="nc-rel" value={rel} onChange={setRel} options={[{ v: "", l: "不填" }, ...Object.keys(REL).filter((r) => r !== "me").map((v) => ({ v, l: REL[v] }))]} /></Field>
          <Field orientation="horizontal" className="justify-between"><FieldLabel>组织</FieldLabel><Select testId="nc-org" value={o} onChange={setO} options={[{ v: "", l: "无" }, ...(orgs.data ?? []).map((x) => ({ v: x.id, l: x.name }))]} /></Field>
          {o && <Field><FieldLabel htmlFor="nc-title">职位（可选）</FieldLabel><Input id="nc-title" className="rounded-lg" maxLength={80} value={title} onChange={(e) => setTitle(e.target.value)} data-nc-title /></Field>}
          {accts.data && <Field><FieldLabel htmlFor="nc-mail">邮箱（可选）</FieldLabel><Input id="nc-mail" type="email" className="rounded-lg" value={mail} onChange={(e) => setMail(e.target.value)} placeholder="会作为这个联系人的一个账号" data-nc-mail /></Field>}
        </FieldGroup>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={onClose}>取消</Button><Button disabled={!n.trim() || busy} data-testid="nc-go" onClick={() => void create()}>建立</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
