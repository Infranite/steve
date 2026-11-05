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
import { ContactAvatar, Loading, act, joinList, useLoad } from "@/people/common"
import { t as tr, useT } from "@/i18n"
import { orgLine } from "@/people/contacts"
import type { Wait } from "@/store"

/* 需要你判断 (domain-model.md §2.3): what Steve could not settle by itself — weak-evidence merges and people still
 * without a name after the lookups. Nobody is shown by a raw id: a nameless person is “未知飞书用户” with where they were
 * seen and what they said last. People only seen in groups you set to 不提醒 / 屏蔽 are left out by the Hub. */
type Order = "active" | "space"
let corder: Order = "active"
export function PendingContacts() {
  const t = useT()
  const { data, err, reload } = useLoad(() => people.pending(), [])
  const [query, setQuery] = useState(""), [filter, setFilter] = useState("all"), [progress, setProgress] = useState(""), [failure, setFailure] = useState("")
  const [retry, setRetry] = useState<{ ids: string[]; run: (id: string) => Promise<unknown>; ok: (n: number) => string } | null>(null)
  const [order, setOrder] = useState<Order>(corder), [sel, setSel] = useState<string[]>([]), [busy, setBusy] = useState(false)
  if (err) return <Empty title={t("people:pending.loadFailed")} sub={err.message} />
  if (!data) return <Loading />
  const ms = data.proposals, unk = data.unconfirmed, quiet = data.quiet ?? 0
  if (!ms.length && !unk.length) return <Empty title={t("people:pending.nobody")} sub={quiet ? t("people:pending.quiet", { count: quiet }) : t("people:pending.nobodySub")} />
  const visible = unk.filter((c) => (filter === "all" || (filter === "nameless" ? isNameless(c) : !isNameless(c))) && [labelOf(c), c.name, orgLine(c), c.account?.system_name, ...(c.seen?.spaces.map((x) => x.name) ?? [])].join(" ").toLocaleLowerCase().includes(query.trim().toLocaleLowerCase()))
  const selected = sel.filter((id) => unk.some((c) => c.id === id))
  const named = visible.filter((c) => !isNameless(c))
  const batch = async (ids: string[], f: (id: string) => Promise<unknown>, ok: (n: number) => string) => {
    setBusy(true); setFailure(""); setRetry(null)
    const failed: string[] = []; let n = 0, next = 0
    // Bound concurrency so large directories do not flood the API.
    await Promise.all(Array.from({ length: Math.min(4, ids.length) }, async () => {
      while (next < ids.length) { const id = ids[next++]; try { await f(id) } catch { failed.push(id) }; setProgress(`${++n}/${ids.length}`) }
    }))
    setSel(failed); await reload(); setBusy(false); setProgress("")
    if (failed.length) { setFailure(t("people:pending.failed", { count: failed.length })); setRetry({ ids: failed, run: f, ok }) }
    toast(failed.length ? t("people:pending.partial", { done: ids.length - failed.length, total: ids.length }) : ok(ids.length))
  }
  const ignore = (ids: string[]) => batch(ids, (id) => people.patchContact(id, { kind: "virtual" }), (count) => tr("people:pending.ignoredN", { count }))
  const toggle = (id: string, on: boolean) => setSel((prev) => on ? [...new Set([...prev, id])] : prev.filter((x) => x !== id))
  const groups: [string, PendingContact[]][] = order === "active" ? [["", visible]] : bySpace(visible)
  return (
    <div className="flex flex-col gap-6" data-pending>
      {ms.length > 0 && <section><SectionLabel>{t("people:pending.sameHead", { n: ms.length })}</SectionLabel><div className="flex flex-col gap-3">{ms.map((m) => <ProposalCard key={m.id} m={m} />)}</div></section>}
      {unk.length > 0 && <section data-unknown-list>
        <SectionLabel right={<Segmented size="sm" value={order} onChange={(v) => { corder = v; setOrder(v) }} options={[["active", t("people:pending.order.active")], ["space", t("people:pending.order.space")]] as const} />}>{t("people:pending.judgeHead", { n: unk.length })}</SectionLabel>
        <div className="mb-3 flex flex-wrap items-center gap-2">
          <Input className="min-w-40 flex-1" aria-label={t("people:pending.searchAria")} placeholder={t("people:pending.searchPh")} value={query} disabled={busy} onChange={(e) => setQuery(e.target.value)} />
          <select className="h-9 rounded-md border bg-background px-2 text-sm" aria-label={t("people:pending.filterAria")} value={filter} disabled={busy} onChange={(e) => setFilter(e.target.value)}><option value="all">{t("people:pending.filter.all")}</option><option value="nameless">{t("people:pending.filter.nameless")}</option><option value="named">{t("people:pending.filter.named")}</option></select>
        </div>
        <div className="mb-3 flex flex-wrap items-center gap-2" role="toolbar" aria-label={t("people:pending.bulk")}>
          <Button size="sm" variant="outline" disabled={busy || !visible.length} onClick={() => setSel((prev) => [...new Set([...prev, ...visible.map((c) => c.id)])])}>{t("people:pending.selectAll", { n: visible.length })}</Button>
          <Button size="sm" disabled={busy || !named.length} onClick={() => void batch(named.map((c) => c.id), (id) => people.confirm(id), (count) => tr("people:pending.confirmedN", { count }))} data-confirm-all>{t("people:pending.confirmNamed", { n: named.length })}</Button>
          <Button size="sm" variant="outline" disabled={busy || !selected.length} onClick={() => void ignore(selected)} data-ignore-selected>{t("people:pending.ignoreSel", { n: selected.length })}</Button>
          {selected.length > 0 && <Button size="sm" variant="ghost" disabled={busy} onClick={() => setSel([])}>{t("people:pending.unselect")}</Button>}
          <span className="type-footnote text-muted-foreground" role="status">{busy ? t("people:pending.working", { progress }) : t("people:pending.selected", { count: selected.length })}</span>
        </div>
        {failure && <div role="alert" className="mb-3 text-sm text-destructive">{failure}{retry && <Button size="sm" variant="outline" disabled={busy} onClick={() => void batch(retry.ids, retry.run, retry.ok)}>{t("people:pending.retryFailed")}</Button>}<Button size="sm" variant="ghost" disabled={busy} onClick={() => { setQuery(""); setFilter("all") }}>{t("people:pending.showFailed")}</Button></div>}
        {!visible.length && <Empty title={t("people:shared.noMatch")} sub={t("people:pending.noMatchSub")} />}
        <div className="flex flex-col gap-4">{groups.map(([g, xs]) => (
          <div key={g || "all"} data-pending-group={g || undefined}>
            {g && <div className="mb-1.5 px-1 type-footnote font-medium text-muted-foreground">{g === NEVER_SEEN ? t("people:pending.neverSeen") : g} · {xs.length}</div>}
            <Group>{xs.map((c) => <UnknownRow key={c.id} c={c} checked={sel.includes(c.id)} onCheck={(v) => toggle(c.id, v)} busy={busy} onChanged={() => void reload()} />)}</Group>
          </div>))}</div>
        <div className="mt-2 px-1 type-footnote text-muted-foreground">{t("people:pending.note")}{quiet > 0 && " " + t("people:pending.quiet", { count: quiet })}</div>
      </section>}
    </div>
  )
}

const isNameless = (c: PendingContact) => c.nameless ?? looksLikeHandle(c.name)
const labelOf = (c: PendingContact) => c.label ?? (isNameless(c) ? tr("people:pending.unknown") : c.name)
/** by the space they were most recently seen in; never-seen ones last */
/* the group of people never seen in a space: a key no space name can be, labelled where it is drawn */
const NEVER_SEEN = "\u0000never"
function bySpace(xs: PendingContact[]): [string, PendingContact[]][] {
  const m = new Map<string, PendingContact[]>()
  for (const c of xs) { const k = c.seen?.spaces[0]?.name || NEVER_SEEN; m.set(k, [...(m.get(k) ?? []), c]) }
  return [...m.entries()].sort((a, b) => (a[0] === NEVER_SEEN ? 1 : b[0] === NEVER_SEEN ? -1 : b[1].length - a[1].length))
}

function UnknownRow({ c, checked, onCheck, busy, onChanged }: { c: PendingContact; checked: boolean; onCheck: (v: boolean) => void; busy: boolean; onChanged: () => void }) {
  const t = useT()
  const [merge, setMerge] = useState(false)
  const nameless = isNameless(c), label = labelOf(c), seen = c.seen, url = (c.avatar as { url?: string } | null)?.url
  const where = seen && joinList(seen.spaces.map((x) => (x.kind === "dm" ? t("people:pending.dm", { name: x.name }) : x.name)))
  return (
    <Row data-unconfirmed={label} data-nameless={nameless || undefined}
      icon={<span className="flex items-center gap-2.5"><Checkbox aria-label={t("people:pending.select", { name: label })} checked={checked} disabled={busy} onCheckedChange={(v) => onCheck(!!v)} />
        {url ? <img src={url} alt="" className="size-8 rounded-full object-cover" /> : <ContactAvatar name={nameless ? "?" : label} unconfirmed size={32} />}</span>}
      title={<span className="inline-flex min-w-0 items-center gap-1.5"><a href={"#/contacts/" + c.id} className="truncate hover:underline">{label}</a>{orgLine(c) && <span className="truncate type-footnote font-normal text-muted-foreground">{orgLine(c)}</span>}</span>}
      sub={<span className="flex min-w-0 flex-col gap-0.5">
        {where ? <span className="truncate">{seen && seen.messages > 0 ? t("people:pending.seenInMsgs", { where, count: seen.messages }) : t("people:pending.seenIn", { where })}</span> : <span>{c.account ? t("people:pending.notSeenSystem", { system: c.account.system_name }) : t("people:pending.notSeenAccounts", { count: c.accounts })}</span>}
        {seen?.last && <span className="truncate text-foreground/70" data-last-message>{seen.last.space_name ? t("people:pending.lastIn", { text: seen.last.text, time: tm(seen.last.created), space: seen.last.space_name }) : t("people:pending.last", { text: seen.last.text, time: tm(seen.last.created) })}</span>}
      </span>}>
      {merge && <MergeDialog from={c} onClose={() => setMerge(false)} />}
      <span className="flex shrink-0 flex-wrap justify-end gap-1.5">
        {!nameless && <Button size="xs" variant="outline" disabled={busy} onClick={async () => { if (await act(people.confirm(c.id), t("people:pending.confirmedOne", { name: label }))) onChanged() }} data-confirm-new={label}>{t("common:action.confirm")}</Button>}
        <Button size="xs" variant="ghost" disabled={busy} onClick={() => setMerge(true)} data-merge-into={label}>{t("people:pending.who")}</Button>
        <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={async () => { if (await act(people.patchContact(c.id, { kind: "virtual" }), t("people:pending.ignoredOne", { name: label }))) onChanged() }} data-ignore={label}>{t("people:pending.ignore")}</Button>
      </span>
    </Row>
  )
}

/* a proposal is answered through its decision when it has one (the Hub's handler merges and closes the card in 待处理);
 * without one, “合并” is the manual merge and “不是同一个人” has nothing to record, so it is not offered */
function ProposalCard({ m }: { m: MergeProposal }) {
  const t = useT()
  const [busy, setBusy] = useState(false)
  const run = async (p: Promise<unknown>, ok: string) => { setBusy(true); await act(p, ok); setBusy(false) }
  const merge = () => run(m.decision_id ? api.decide(m.decision_id, "merge") : people.merge(m.from.id, m.into.id), t("people:pending.mergedSplit", { name: m.into.name }))
  return (
    <div className="rounded-xl border px-4 py-3" data-merge={m.id}>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <span className="inline-flex min-w-0 items-center gap-2"><ContactAvatar name={m.from.name} unconfirmed={m.from.kind === "unconfirmed"} size={32} /><span className="truncate type-callout font-mono text-[13px]">{m.account?.handle ?? m.from.name}</span></span>
        <ChevronRightIcon className="size-4 text-tertiary" aria-label={t("people:pending.maybe")} />
        <a href={"#/contacts/" + m.into.id} className="inline-flex min-w-0 items-center gap-2 hover:underline"><ContactAvatar name={m.into.name} size={32} /><span className="type-body">{m.into.name}</span></a>
        {orgLine(m.into) && <span className="type-footnote text-muted-foreground">{orgLine(m.into)}</span>}
      </div>
      {m.reason && <div className="mt-2 type-footnote text-muted-foreground">{t("people:shared.reason", { reason: m.reason })}</div>}
      <div className="mt-3 flex justify-end gap-2">
        {m.decision_id && <Button size="sm" variant="ghost" disabled={busy} onClick={() => void run(api.decide(m.decision_id!, "not_same"), t("people:pending.notedNotSame"))} data-merge-no>{t("people:shared.notSame")}</Button>}
        <Button size="sm" disabled={busy} onClick={() => void merge()} data-merge-yes>{t("people:shared.mergeInto", { name: m.into.name })}</Button>
      </div>
    </div>
  )
}

/** `from` → another contact, chosen by search */
export function MergeDialog({ from, onClose }: { from: Pick<Contact, "id" | "name"> & { label?: string }; onClose: () => void }) {
  const t = useT()
  const [q, setQ] = useState(""), [into, setInto] = useState<string | null>(null), [busy, setBusy] = useState(false)
  const cs = useLoad(() => people.contacts({ q: q.trim(), limit: 50 }), [q])
  const xs = (cs.data ?? []).filter((c) => c.id !== from.id)
  const target = xs.find((c) => c.id === into)
  const merge = async () => {
    if (!into) return
    setBusy(true); const r = await act(people.merge(from.id, into), t("people:merge.done", { name: target?.name ?? t("people:shared.contacts") })); setBusy(false)
    if (r) { onClose(); go("#/contacts/" + r.into.id) }
  }
  return (
    <Dialog open onOpenChange={(x) => !x && onClose()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[460px]">
        <DialogHeader><DialogTitle className="type-headline">{t("people:merge.title", { name: nameOf(from) })}</DialogTitle><DialogDescription className="type-callout">{t("people:merge.desc")}</DialogDescription></DialogHeader>
        <Input autoFocus className="rounded-lg" aria-label={t("people:shared.searchContacts")} placeholder={t("people:shared.searchContactsPh")} value={q} onChange={(e) => setQ(e.target.value)} data-merge-search />
        <div className="max-h-64 overflow-y-auto rounded-lg border" role="radiogroup" aria-label={t("people:merge.into")}>
          {!cs.data ? <Loading /> : !xs.length ? <div className="px-3 py-3 type-callout text-muted-foreground">{t("people:shared.noMatch")}</div> : xs.map((c) => (
            <button key={c.id} type="button" role="radio" aria-checked={into === c.id} onClick={() => setInto(c.id)} className="flex w-full items-center gap-2.5 px-3 py-2 text-left hover:bg-accent aria-checked:bg-blue/5" data-merge-opt={c.name}>
              <ContactAvatar name={c.name} size={20} /><span className="type-callout">{c.is_me ? t("people:shared.withMe", { name: c.name }) : c.name}</span><span className="ml-auto type-footnote text-muted-foreground">{orgLine(c)}</span></button>))}
        </div>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button><Button disabled={!into || busy} onClick={() => void merge()} data-merge-go>{target ? t("people:shared.mergeInto", { name: target.name }) : t("people:shared.merge")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/* ───────── 待处理 cards ───────── */
/** the line under the question, by subject_type (contacts.md §0.1 point 6) */
const subjectLine = (s: string) => (s === "party_merge" || s === "attr_value" || s === "attr_def" || s === "mem" ? tr(`people:decision.${s}`) : undefined)
/** party_merge, and every `confirm` (the one kind the modules add; other modules' subjects get the plain yes / no) */
export const isPeopleDecision = (w: Pick<Wait, "type" | "subject">) => w.subject === "party_merge" || w.type === "confirm"
/** the body and buttons of a contacts decision; DecisionCard draws the header */
export function PeopleDecisionBody({ w, busy, act: choose }: { w: Wait; busy: boolean; act: (choice: string) => void }) {
  const t = useT()
  const foot = (c: ReactNode) => <CardFooter className="flex-wrap justify-end gap-2 border-t px-3 py-2.5!">{c}</CardFooter>
  const merge = w.subject === "party_merge", line = subjectLine(w.subject)
  return <>
    <CardContent className="pb-4">
      {line && <div className="type-footnote text-muted-foreground">{line}</div>}
      {w.draft && <div className="mt-1.5 break-words type-body text-muted-foreground">{merge ? t("people:shared.reason", { reason: w.draft }) : w.draft}</div>}
      {merge && <button type="button" onClick={() => go("#/contacts")} className="mt-1.5 type-footnote text-muted-foreground hover:text-foreground hover:underline">{t("people:decision.seePending")}</button>}
    </CardContent>
    {foot(merge
      ? <><Button variant="outline" disabled={busy} onClick={() => choose("not_same")} data-merge-no>{t("people:shared.notSame")}</Button><Button disabled={busy} onClick={() => choose("merge")} data-merge-yes>{t("people:shared.merge")}</Button></>
      : <><Button variant="outline" disabled={busy} onClick={() => choose("no")} data-confirm-no>{t("people:shared.wrong")}</Button><Button disabled={busy} onClick={() => choose("yes")} data-confirm-yes>{t("common:action.confirm")}</Button></>)}
  </>
}
