/* 待确认: merge proposals and unconfirmed contacts (prototype people.tsx PendingContacts), the manual merge dialog,
 * and the 待处理 cards of the contacts decisions (party_merge, and `confirm` on attr_value / attr_def / mem). */
import { useState, type ReactNode } from "react"
import { ChevronRightIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { CardContent, CardFooter } from "@/components/ui/card"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { api } from "@/api"
import { Checkbox } from "@/components/ui/checkbox"
import { go, Group, Row, SectionLabel, Segmented, Empty } from "@/ui"
import { looksLikeHandle, nameOf, people, type Contact, type MergeProposal, type PendingContact } from "@/api/people"
import { tm, toast } from "@/store"
import { ContactAvatar, Loading, act, useLoad } from "@/people/common"
import { orgLine } from "@/people/contacts"
import type { Wait } from "@/store"

/* 需要你判断 (domain-model.md §2.3): what Steve could not settle by itself — weak-evidence merges and people still
 * without a name after the lookups. Nobody is shown by a raw id: a nameless person is “未知飞书用户” with where they were
 * seen and what they said last. People only seen in groups you set to 不提醒 / 屏蔽 are left out by the Hub. */
type Order = "活跃" | "空间"
let corder: Order = "活跃"
export function PendingContacts() {
  const { data, err, reload } = useLoad(() => people.pending(), [])
  const [query, setQuery] = useState(""), [filter, setFilter] = useState("all"), [progress, setProgress] = useState(""), [failure, setFailure] = useState("")
  const [retry, setRetry] = useState<{ ids: string[]; run: (id: string) => Promise<unknown>; ok: string } | null>(null)
  const [order, setOrder] = useState<Order>(corder), [sel, setSel] = useState<string[]>([]), [busy, setBusy] = useState(false)
  if (err) return <Empty title="待确认载入失败" sub={err.message} />
  if (!data) return <Loading />
  const ms = data.proposals, unk = data.unconfirmed, quiet = data.quiet ?? 0
  if (!ms.length && !unk.length) return <Empty title="没有需要你判断的人" sub={quiet ? `另有 ${quiet} 个只出现在不提醒或屏蔽的群里，没有列出。` : "认得出名字的人 Steve 会直接建好；认不出、拿不准的才放在这里。"} />
  const visible = unk.filter((c) => (filter === "all" || (filter === "nameless" ? isNameless(c) : !isNameless(c))) && [labelOf(c), c.name, orgLine(c), c.account?.system_name, ...(c.seen?.spaces.map((x) => x.name) ?? [])].join(" ").toLocaleLowerCase().includes(query.trim().toLocaleLowerCase()))
  const selected = sel.filter((id) => unk.some((c) => c.id === id))
  const named = visible.filter((c) => !isNameless(c))
  const batch = async (ids: string[], f: (id: string) => Promise<unknown>, ok: string) => {
    setBusy(true); setFailure(""); setRetry(null)
    const failed: string[] = []; let n = 0, next = 0
    // Bound concurrency so large directories do not flood the API.
    await Promise.all(Array.from({ length: Math.min(4, ids.length) }, async () => {
      while (next < ids.length) { const id = ids[next++]; try { await f(id) } catch { failed.push(id) }; setProgress(`${++n}/${ids.length}`) }
    }))
    setSel(failed); await reload(); setBusy(false); setProgress("")
    if (failed.length) { setFailure(`${failed.length} 个操作失败，已保留选择，可重试。`); setRetry({ ids: failed, run: f, ok }) }
    toast(failed.length ? `${ids.length - failed.length}/${ids.length} 个完成` : ok.replace("N", String(ids.length)))
  }
  const ignore = (ids: string[]) => batch(ids, (id) => people.patchContact(id, { kind: "virtual" }), "已忽略 N 个（可在联系人页的虚拟账号中恢复）")
  const toggle = (id: string, on: boolean) => setSel((prev) => on ? [...new Set([...prev, id])] : prev.filter((x) => x !== id))
  const groups: [string, PendingContact[]][] = order === "活跃" ? [["", visible]] : bySpace(visible)
  return (
    <div className="flex flex-col gap-6" data-pending>
      {ms.length > 0 && <section><SectionLabel>可能是同一个人 · {ms.length}</SectionLabel><div className="flex flex-col gap-3">{ms.map((m) => <ProposalCard key={m.id} m={m} />)}</div></section>}
      {unk.length > 0 && <section data-unknown-list>
        <SectionLabel right={<Segmented size="sm" value={order} onChange={(v) => { corder = v; setOrder(v) }} options={[["活跃", "按活跃"], ["空间", "按空间"]] as const} />}>需要你判断 · {unk.length}</SectionLabel>
        <div className="mb-3 flex flex-wrap items-center gap-2">
          <Input className="min-w-40 flex-1" aria-label="搜索待确认联系人" placeholder="搜索名字、组织或空间" value={query} disabled={busy} onChange={(e) => setQuery(e.target.value)} />
          <select className="h-9 rounded-md border bg-background px-2 text-sm" aria-label="联系人筛选" value={filter} disabled={busy} onChange={(e) => setFilter(e.target.value)}><option value="all">全部</option><option value="nameless">未知用户</option><option value="named">已有名字</option></select>
        </div>
        <div className="mb-3 flex flex-wrap items-center gap-2" role="toolbar" aria-label="批量操作">
          <Button size="sm" variant="outline" disabled={busy || !visible.length} onClick={() => setSel((prev) => [...new Set([...prev, ...visible.map((c) => c.id)])])}>全选筛选结果（{visible.length}）</Button>
          <Button size="sm" disabled={busy || !named.length} onClick={() => void batch(named.map((c) => c.id), (id) => people.confirm(id), "已确认 N 个联系人")} data-confirm-all>确认有名字的（{named.length}）</Button>
          <Button size="sm" variant="outline" disabled={busy || !selected.length} onClick={() => void ignore(selected)} data-ignore-selected>忽略所选（{selected.length}）</Button>
          {selected.length > 0 && <Button size="sm" variant="ghost" disabled={busy} onClick={() => setSel([])}>取消选择</Button>}
          <span className="type-footnote text-muted-foreground" role="status">{busy ? `处理中 ${progress}` : `已选 ${selected.length} 个（包含其他筛选下的选择）。忽略后可在虚拟账号中恢复。`}</span>
        </div>
        {failure && <div role="alert" className="mb-3 text-sm text-destructive">{failure}{retry && <Button size="sm" variant="outline" disabled={busy} onClick={() => void batch(retry.ids, retry.run, retry.ok)}>重试失败项</Button>}<Button size="sm" variant="ghost" disabled={busy} onClick={() => { setQuery(""); setFilter("all") }}>查看失败项</Button></div>}
        {!visible.length && <Empty title="没有匹配的联系人" sub="试试其他关键词或筛选条件。" />}
        <div className="flex flex-col gap-4">{groups.map(([g, xs]) => (
          <div key={g || "all"} data-pending-group={g || undefined}>
            {g && <div className="mb-1.5 px-1 type-footnote font-medium text-muted-foreground">{g} · {xs.length}</div>}
            <Group>{xs.map((c) => <UnknownRow key={c.id} c={c} checked={sel.includes(c.id)} onCheck={(v) => toggle(c.id, v)} busy={busy} onChanged={() => void reload()} />)}</Group>
          </div>))}</div>
        <div className="mt-2 px-1 type-footnote text-muted-foreground">未确认的人照常处理消息，但不会被当作熟人对待，例如不会套用“同事”的响应规则。{quiet > 0 && ` 另有 ${quiet} 个只出现在不提醒或屏蔽的群里，没有列出。`}</div>
      </section>}
    </div>
  )
}

const isNameless = (c: PendingContact) => c.nameless ?? looksLikeHandle(c.name)
const labelOf = (c: PendingContact) => c.label ?? (isNameless(c) ? "未知用户" : c.name)
/** by the space they were most recently seen in; never-seen ones last */
function bySpace(xs: PendingContact[]): [string, PendingContact[]][] {
  const m = new Map<string, PendingContact[]>()
  for (const c of xs) { const k = c.seen?.spaces[0]?.name || "没有见过消息"; m.set(k, [...(m.get(k) ?? []), c]) }
  return [...m.entries()].sort((a, b) => (a[0] === "没有见过消息" ? 1 : b[0] === "没有见过消息" ? -1 : b[1].length - a[1].length))
}

function UnknownRow({ c, checked, onCheck, busy, onChanged }: { c: PendingContact; checked: boolean; onCheck: (v: boolean) => void; busy: boolean; onChanged: () => void }) {
  const [merge, setMerge] = useState(false)
  const nameless = isNameless(c), label = labelOf(c), seen = c.seen, url = (c.avatar as { url?: string } | null)?.url
  const where = seen?.spaces.map((x) => (x.kind === "dm" ? `和 ${x.name} 的私聊` : x.name)).join("、")
  return (
    <Row data-unconfirmed={label} data-nameless={nameless || undefined}
      icon={<span className="flex items-center gap-2.5"><Checkbox aria-label={`选中 ${label}`} checked={checked} disabled={busy} onCheckedChange={(v) => onCheck(!!v)} />
        {url ? <img src={url} alt="" className="size-8 rounded-full object-cover" /> : <ContactAvatar name={nameless ? "?" : label} unconfirmed size={32} />}</span>}
      title={<span className="inline-flex min-w-0 items-center gap-1.5"><a href={"#/contacts/" + c.id} className="truncate hover:underline">{label}</a>{orgLine(c) && <span className="truncate type-footnote font-normal text-muted-foreground">{orgLine(c)}</span>}</span>}
      sub={<span className="flex min-w-0 flex-col gap-0.5">
        {where ? <span className="truncate">出现在 {where}{seen && seen.messages > 0 ? ` · ${seen.messages} 条消息` : ""}</span> : <span>{c.account ? `${c.account.system_name}账号` : `${c.accounts} 个账号`}，还没有见过消息</span>}
        {seen?.last && <span className="truncate text-foreground/70" data-last-message>“{seen.last.text}” · {tm(seen.last.created)}{seen.last.space_name ? ` · ${seen.last.space_name}` : ""}</span>}
      </span>}>
      {merge && <MergeDialog from={c} onClose={() => setMerge(false)} />}
      <span className="flex shrink-0 flex-wrap justify-end gap-1.5">
        {!nameless && <Button size="xs" variant="outline" disabled={busy} onClick={async () => { if (await act(people.confirm(c.id), `已确认 ${label}`)) onChanged() }} data-confirm-new={label}>确认</Button>}
        <Button size="xs" variant="ghost" disabled={busy} onClick={() => setMerge(true)} data-merge-into={label}>是谁…</Button>
        <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={async () => { if (await act(people.patchContact(c.id, { kind: "virtual" }), `已忽略 ${label}`)) onChanged() }} data-ignore={label}>忽略</Button>
      </span>
    </Row>
  )
}

/* a proposal is answered through its decision when it has one (the Hub's handler merges and closes the card in 待处理);
 * without one, “合并” is the manual merge and “不是同一个人” has nothing to record, so it is not offered */
function ProposalCard({ m }: { m: MergeProposal }) {
  const [busy, setBusy] = useState(false)
  const run = async (p: Promise<unknown>, ok: string) => { setBusy(true); await act(p, ok); setBusy(false) }
  const merge = () => run(m.decision_id ? api.decide(m.decision_id, "merge") : people.merge(m.from.id, m.into.id), `已合并到 ${m.into.name}，可以在 ${m.into.name} 的页面里拆回`)
  return (
    <div className="rounded-xl border px-4 py-3" data-merge={m.id}>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <span className="inline-flex min-w-0 items-center gap-2"><ContactAvatar name={m.from.name} unconfirmed={m.from.kind === "unconfirmed"} size={32} /><span className="truncate type-callout font-mono text-[13px]">{m.account?.handle ?? m.from.name}</span></span>
        <ChevronRightIcon className="size-4 text-tertiary" aria-label="可能是" />
        <a href={"#/contacts/" + m.into.id} className="inline-flex min-w-0 items-center gap-2 hover:underline"><ContactAvatar name={m.into.name} size={32} /><span className="type-body">{m.into.name}</span></a>
        {orgLine(m.into) && <span className="type-footnote text-muted-foreground">{orgLine(m.into)}</span>}
      </div>
      {m.reason && <div className="mt-2 type-footnote text-muted-foreground">依据：{m.reason}</div>}
      <div className="mt-3 flex justify-end gap-2">
        {m.decision_id && <Button size="sm" variant="ghost" disabled={busy} onClick={() => void run(api.decide(m.decision_id!, "not_same"), "已记下：不是同一个人")} data-merge-no>不是同一个人</Button>}
        <Button size="sm" disabled={busy} onClick={() => void merge()} data-merge-yes>合并到 {m.into.name}</Button>
      </div>
    </div>
  )
}

/** `from` → another contact, chosen by search */
export function MergeDialog({ from, onClose }: { from: Pick<Contact, "id" | "name"> & { label?: string }; onClose: () => void }) {
  const [q, setQ] = useState(""), [into, setInto] = useState<string | null>(null), [busy, setBusy] = useState(false)
  const cs = useLoad(() => people.contacts({ q: q.trim(), limit: 50 }), [q])
  const xs = (cs.data ?? []).filter((c) => c.id !== from.id)
  const target = xs.find((c) => c.id === into)
  const merge = async () => {
    if (!into) return
    setBusy(true); const r = await act(people.merge(from.id, into), `已合并到 ${target?.name ?? "联系人"}，可以在那里拆回`); setBusy(false)
    if (r) { onClose(); go("#/contacts/" + r.into.id) }
  }
  return (
    <Dialog open onOpenChange={(x) => !x && onClose()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[460px]">
        <DialogHeader><DialogTitle className="type-headline">把 {nameOf(from)} 合并到…</DialogTitle><DialogDescription className="type-callout">资料、账号、记忆、团队和事项关联都改挂到选中的联系人；合并之后可以拆回。</DialogDescription></DialogHeader>
        <Input autoFocus className="rounded-lg" aria-label="搜索联系人" placeholder="搜索名字、组织、邮箱" value={q} onChange={(e) => setQ(e.target.value)} data-merge-search />
        <div className="max-h-64 overflow-y-auto rounded-lg border" role="radiogroup" aria-label="合并到">
          {!cs.data ? <Loading /> : !xs.length ? <div className="px-3 py-3 type-callout text-muted-foreground">没有匹配的联系人</div> : xs.map((c) => (
            <button key={c.id} type="button" role="radio" aria-checked={into === c.id} onClick={() => setInto(c.id)} className="flex w-full items-center gap-2.5 px-3 py-2 text-left hover:bg-accent aria-checked:bg-blue/5" data-merge-opt={c.name}>
              <ContactAvatar name={c.name} size={20} /><span className="type-callout">{c.is_me ? c.name + "（我）" : c.name}</span><span className="ml-auto type-footnote text-muted-foreground">{orgLine(c)}</span></button>))}
        </div>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={onClose}>取消</Button><Button disabled={!into || busy} onClick={() => void merge()} data-merge-go>{target ? `合并到 ${target.name}` : "合并"}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/* ───────── 待处理 cards ───────── */
/** the line under the question, by subject_type (contacts.md §0.1 point 6) */
const SUBJECT: Record<string, string> = {
  party_merge: "两个联系人可能是同一个人。合并后资料、账号和记忆都归到一起，可以拆回。",
  attr_value: "Steve 从往来里推断出的联系人资料，确认后才会当作事实使用。",
  attr_def: "Steve 提议新增一种联系人资料。",
  mem: "Steve 想记住这件事，确认后才会用到。",
}
/** party_merge, and every `confirm` (the one kind the modules add; other modules' subjects get the plain yes / no) */
export const isPeopleDecision = (w: Pick<Wait, "type" | "subject">) => w.subject === "party_merge" || w.type === "confirm"
/** the body and buttons of a contacts decision; DecisionCard draws the header */
export function PeopleDecisionBody({ w, busy, act: choose }: { w: Wait; busy: boolean; act: (choice: string) => void }) {
  const foot = (c: ReactNode) => <CardFooter className="flex-wrap justify-end gap-2 border-t px-3 py-2.5!">{c}</CardFooter>
  const merge = w.subject === "party_merge"
  return <>
    <CardContent className="pb-4">
      {SUBJECT[w.subject] && <div className="type-footnote text-muted-foreground">{SUBJECT[w.subject]}</div>}
      {w.draft && <div className="mt-1.5 break-words type-body text-muted-foreground">{merge ? "依据：" : ""}{w.draft}</div>}
      {merge && <button type="button" onClick={() => go("#/contacts")} className="mt-1.5 type-footnote text-muted-foreground hover:text-foreground hover:underline">在联系人的“待确认”里查看</button>}
    </CardContent>
    {foot(merge
      ? <><Button variant="outline" disabled={busy} onClick={() => choose("not_same")} data-merge-no>不是同一个人</Button><Button disabled={busy} onClick={() => choose("merge")} data-merge-yes>合并</Button></>
      : <><Button variant="outline" disabled={busy} onClick={() => choose("no")} data-confirm-no>不对</Button><Button disabled={busy} onClick={() => choose("yes")} data-confirm-yes>确认</Button></>)}
  </>
}
