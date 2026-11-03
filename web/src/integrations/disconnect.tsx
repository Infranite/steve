/* 断开 and 清理记录 on the 接入 page, for any provider. Disconnecting an identity stops it (credential destroyed, long
 * connection or poller stopped, settings removed); its groups and messages stay unless you choose a cleanup level.
 * A cleanup always shows a dry run first (counts, what is kept and why) and needs an explicit confirmation; the Hub
 * backs up the database before it purges. Data: DELETE /connections/:id, /connections/:id/usage, /channels/cleanup*. */
import { useEffect, useState } from "react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { isNotWired } from "@/api"
import { toast } from "@/store"
import { report } from "@/accounts/load"
import { accountsApi, type RUsage } from "@/api/accounts"
import { cleanupApi, LEVEL_TEXT, type CleanupLevel, type CleanupPlan, type CleanupTarget } from "@/api/cleanup"

/** one identity being disconnected: `stops` says what it no longer does */
export type Leaving = { id: string; label: string; stops: string }
type Choice = "keep" | CleanupLevel

function sum(plans: CleanupPlan[]): CleanupPlan {
  return plans.reduce((a, p) => ({ ...a, messages_purged: (a.messages_purged ?? 0) + (p.messages_purged ?? 0), decisions: (a.decisions ?? 0) + (p.decisions ?? 0), decision_ids: [...(a.decision_ids ?? []), ...(p.decision_ids ?? [])], spaces: a.spaces + p.spaces, messages: a.messages + p.messages, contacts: a.contacts + p.contacts, accounts: a.accounts + p.accounts, kept: [...a.kept, ...p.kept], kept_count: a.kept_count + p.kept_count }),
    { previews: plans, name: plans[0]?.name ?? "", spaces: 0, messages: 0, contacts: 0, accounts: 0, kept: [], kept_count: 0 })
}

/** “将删除 N 个群、M 条消息、K 个联系人；保留 X 个（…）” */
export function PlanSummary({ plan, level }: { plan: CleanupPlan; level: CleanupLevel }) {
  const gone = level === "text"
    ? [`${plan.messages} 条消息的正文和附件`]
    : [`${plan.spaces} 个群`, `${plan.messages} 条消息`, `${plan.contacts} 个自动识别出的联系人`, `${plan.decisions ?? 0} 个关联决定`, `${plan.messages_purged ?? 0} 条保留消息的正文和附件`].filter((s) => !s.startsWith("0 "))
  const KIND: Record<string, [string, string]> = { space: ["个群", "群"], message: ["条消息", "消息"], contact: ["个联系人", "联系人"] }
  const keptBy = Object.entries(plan.kept.reduce<Record<string, number>>((m, k) => ({ ...m, [k.kind]: (m[k.kind] ?? 0) + 1 }), {})).map(([k, n]) => `${n} ${KIND[k]?.[0] ?? "项"}`)
  const reasons = [...new Set(plan.kept.map((k) => k.reason))]
  return (
    <div className="flex flex-col gap-2 rounded-lg border px-3 py-2.5 type-callout" data-cleanup-plan data-plan-spaces={plan.spaces} data-plan-messages={plan.messages} data-plan-contacts={plan.contacts} data-plan-kept={plan.kept_count}>
      <div>{gone.length && (plan.messages || plan.spaces || plan.contacts || plan.messages_purged || plan.decisions) ? <>将删除 {gone.join("、")}</> : "没有可以删除的记录"}{plan.kept_count > 0 && <>；保留 {plan.kept.length === plan.kept_count && keptBy.length ? keptBy.join("、") : `${plan.kept_count} 项`}{reasons.length ? `（${reasons.slice(0, 2).join("、")}）` : ""}</>}。</div>
      {(plan.decisions ?? 0) > 0 && <div className="type-footnote text-muted-foreground" data-cleanup-undo-note>删除关联决定后，其自动操作将不能再撤销；自动操作历史仍会保留。</div>}
      {plan.kept.length > 0 && <details className="type-footnote text-muted-foreground"><summary className="cursor-pointer select-none">看保留了哪些</summary>
        <ul className="mt-1 flex max-h-40 flex-col gap-0.5 overflow-y-auto">{plan.kept.map((k) => <li key={k.kind + k.id}>{k.name || k.id} · {k.reason}</li>)}</ul></details>}
    </div>
  )
}

function Choices({ value, onChange, allowKeep, offered }: { value: Choice; onChange: (v: Choice) => void; allowKeep: boolean; offered: boolean }) {
  const opts: Choice[] = allowKeep ? ["keep", "text", "all"] : ["text", "all"]
  return (
    <RadioGroup value={value} onValueChange={(v) => onChange(v as Choice)} className="gap-2" aria-label="历史记录">
      {opts.map((o) => (
        <label key={o} className={"flex cursor-pointer items-start gap-2.5 rounded-lg border px-3 py-2.5 " + (value === o ? "border-primary/60 bg-muted/40" : "")} data-cleanup-choice={o}>
          <RadioGroupItem value={o} disabled={o !== "keep" && !offered} className="mt-0.5" />
          <span className="min-w-0"><span className={"type-body " + (o === "all" ? "text-destructive" : "")}>{LEVEL_TEXT[o][0]}</span><span className="block type-footnote text-muted-foreground">{LEVEL_TEXT[o][1]}</span></span>
        </label>))}
      {!offered && <div className="px-1 type-footnote text-muted-foreground">这个 Hub 还不支持清理记录。</div>}
    </RadioGroup>
  )
}

/** the dry run for `targets` at `level`; `undefined` while loading, `null` when the Hub has no cleanup */
function usePlan(targets: CleanupTarget[], level: Choice): CleanupPlan | null | undefined {
  const [plan, setPlan] = useState<CleanupPlan | null | undefined>(undefined)
  const key = JSON.stringify(targets) + level
  useEffect(() => {
    if (level === "keep") { setPlan(undefined); return }
    let stop = false
    setPlan(undefined)
    Promise.all(targets.map((t) => cleanupApi.preview(t, level))).then((ps) => !stop && setPlan(sum(ps)), (e) => { if (stop) return; if (isNotWired(e)) setPlan(null); else { report(e); setPlan(null) } })
    return () => { stop = true }
  }, [key])
  return plan
}

/** 断开: one identity, or the whole installation (all its identities, you first) */
export function DisconnectDialog({ title, name, leaving, keptNote, onClose, onDone }: { title: string; name: string; leaving: Leaving[]; keptNote?: string; onClose: () => void; onDone: () => void }) {
  const [usage, setUsage] = useState<RUsage[] | null>(null), [choice, setChoice] = useState<Choice>("keep"), [typed, setTyped] = useState(""), [busy, setBusy] = useState(false), [offered, setOffered] = useState(true)
  useEffect(() => { Promise.all(leaving.map((l) => accountsApi.usage(l.id))).then(setUsage, () => setUsage([])) }, [])
  useEffect(() => { cleanupApi.preview({ connection_id: leaving[0].id }, "text").catch((e) => { if (isNotWired(e)) setOffered(false) }) }, [])
  const targets = leaving.map((l) => ({ connection_id: l.id }))
  const plan = usePlan(targets, choice)
  const used = (usage ?? []).flatMap((u) => [...u.flows.map((f) => "流程「" + f.name + "」"), ...u.sources.map((x) => "事件源「" + x.name + "」")])
  const spaces = (usage ?? []).reduce((n, u) => n + (u.spaces || 0), 0)
  const needName = choice === "all", ok = !needName || typed.trim() === name.trim()
  const go = async () => {
    setBusy(true)
    try {
      if (choice !== "keep") for (const [i, t] of targets.entries()) await cleanupApi.run(t, choice, plan?.previews?.[i]?.decision_ids)
      for (const l of [...leaving].reverse()) await accountsApi.disconnect(l.id)
      toast(choice === "keep" ? "已断开" : "已断开，并清理了记录"); onDone()
    } catch (e) { report(e) } finally { setBusy(false) }
  }
  return (
    <Dialog open onOpenChange={(o) => !o && !busy && onClose()}>
      <DialogContent className="max-h-[90dvh] gap-4 overflow-y-auto rounded-2xl" style={{ maxWidth: 520 }} data-disconnect-dialog>
        <DialogHeader><DialogTitle className="type-headline">{title}</DialogTitle>
          <DialogDescription className="type-callout">断开后下面这些会停止，凭据从保险箱删除。以后可以重新接入。</DialogDescription></DialogHeader>
        <div className="flex flex-col gap-1.5 type-callout" data-disconnect-effects>
          <div className="font-medium">会停止</div>
          <ul className="list-disc pl-5 text-muted-foreground">
            {leaving.map((l) => <li key={l.id}>{l.label}：{l.stops}</li>)}
            {used.map((u) => <li key={u}>{u}</li>)}
          </ul>
          <div className="mt-1 font-medium">会保留</div>
          <ul className="list-disc pl-5 text-muted-foreground">
            <li>{spaces ? `${spaces} 个群和私聊` : "群和私聊"}标为“已离开”，消息记录还在（除非下面选了删除）</li>
            <li>事项、决定和知识里的引用不受影响</li>
            {keptNote && <li>{keptNote}</li>}
          </ul>
        </div>
        <div className="flex flex-col gap-2"><div className="type-callout font-medium">历史记录</div>
          <Choices value={choice} onChange={setChoice} allowKeep offered={offered} />
          {choice !== "keep" && (plan === undefined ? <div className="flex items-center gap-2 type-footnote text-muted-foreground"><Spinner className="size-3" />正在统计会删除什么…</div> : plan && <PlanSummary plan={plan} level={choice} />)}
          {needName && <label className="flex flex-col gap-1 type-footnote text-muted-foreground">输入“{name}”确认删除<Input className="rounded-lg" value={typed} onChange={(e) => setTyped(e.target.value)} aria-label={`输入 ${name} 确认`} data-cleanup-typed /></label>}
          {choice !== "keep" && <div className="px-1 type-footnote text-muted-foreground">删除前 Steve 会先备份数据库。</div>}
        </div>
        <DialogFooter className="gap-2"><Button variant="outline" disabled={busy} onClick={onClose}>取消</Button>
          <Button variant="destructive" disabled={busy || !usage || !ok || (choice !== "keep" && !plan)} onClick={() => void go()} data-disconnect-confirm>{busy && <Spinner />}{choice === "keep" ? "断开" : "断开并删除"}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/** 清理记录 without disconnecting (or for an identity already disconnected) */
export function CleanupDialog({ target, name, onClose, onDone }: { target: CleanupTarget; name: string; onClose: () => void; onDone: () => void }) {
  const [choice, setChoice] = useState<Choice>("text"), [typed, setTyped] = useState(""), [busy, setBusy] = useState(false)
  const plan = usePlan([target], choice)
  const level = choice as CleanupLevel, ok = level !== "all" || typed.trim() === name.trim()
  const empty = !!plan && !plan.messages && !plan.spaces && !plan.contacts && !plan.messages_purged && !plan.decisions
  const go = async () => {
    setBusy(true)
    try { const r = await cleanupApi.run(target, level, plan?.decision_ids); toast(r.backup ? "已清理，清理前的数据库已备份" : "已清理"); onDone() } catch (e) { report(e) } finally { setBusy(false) }
  }
  return (
    <Dialog open onOpenChange={(o) => !o && !busy && onClose()}>
      <DialogContent className="max-h-[90dvh] gap-4 overflow-y-auto rounded-2xl" style={{ maxWidth: 500 }} data-cleanup-dialog>
        <DialogHeader><DialogTitle className="type-headline">清理“{name}”的记录</DialogTitle>
          <DialogDescription className="type-callout">只清理只经过这个身份看到的群和消息。先看会删除什么，确认后才删除；删除前会备份数据库。</DialogDescription></DialogHeader>
        <Choices value={choice} onChange={setChoice} allowKeep={false} offered />
        {plan === undefined ? <div className="flex items-center gap-2 type-footnote text-muted-foreground"><Spinner className="size-3" />正在统计会删除什么…</div> : plan ? <PlanSummary plan={plan} level={level} /> : <div className="type-footnote text-muted-foreground">这个 Hub 还不支持清理记录。</div>}
        {level === "all" && <label className="flex flex-col gap-1 type-footnote text-muted-foreground">输入“{name}”确认删除<Input className="rounded-lg" value={typed} onChange={(e) => setTyped(e.target.value)} aria-label={`输入 ${name} 确认`} data-cleanup-typed /></label>}
        <DialogFooter className="gap-2"><Button variant="outline" disabled={busy} onClick={onClose}>取消</Button>
          <Button variant="destructive" disabled={busy || !plan || empty || !ok} onClick={() => void go()} data-cleanup-confirm>{busy && <Spinner />}删除</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
