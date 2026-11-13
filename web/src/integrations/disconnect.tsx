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
import { useT } from "@/i18n"
import { toast } from "@/store"
import { report } from "@/accounts/load"
import { accountsApi, type RUsage } from "@/api/accounts"
import { cleanupApi, LEVEL_TEXT, type CleanupLevel, type CleanupPlan, type CleanupTarget } from "@/api/cleanup"
import { SectionLabel, DiscChevron, SUMMARY } from "@/ui"

/** one identity being disconnected: `stops` says what it no longer does */
export type Leaving = { id: string; label: string; stops: string }
type Choice = "keep" | CleanupLevel

function sum(plans: CleanupPlan[]): CleanupPlan {
  return plans.reduce((a, p) => ({ ...a, messages_purged: (a.messages_purged ?? 0) + (p.messages_purged ?? 0), decisions: (a.decisions ?? 0) + (p.decisions ?? 0), decision_ids: [...(a.decision_ids ?? []), ...(p.decision_ids ?? [])], spaces: a.spaces + p.spaces, messages: a.messages + p.messages, contacts: a.contacts + p.contacts, accounts: a.accounts + p.accounts, kept: [...a.kept, ...p.kept], kept_count: a.kept_count + p.kept_count }),
    { previews: plans, name: plans[0]?.name ?? "", spaces: 0, messages: 0, contacts: 0, accounts: 0, kept: [], kept_count: 0 })
}

/** “将删除 N 个群、M 条消息、K 个联系人；保留 X 个（…）” */
export function PlanSummary({ plan, level }: { plan: CleanupPlan; level: CleanupLevel }) {
  const t = useT(), sep = t("integrations:list.sep")
  const gone = level === "text"
    ? [t("integrations:cleanup.goneText", { count: plan.messages })]
    : ([[plan.spaces, "goneSpaces"], [plan.messages, "goneMessages"], [plan.contacts, "goneContacts"], [plan.decisions ?? 0, "goneDecisions"], [plan.messages_purged ?? 0, "gonePurged"]] as const)
      .filter(([n]) => n !== 0).map(([count, k]) => t(`integrations:cleanup.${k}`, { count }))
  // kept rows by kind; the kind is the Hub's value, the label is ours
  const KIND: Record<string, "keptSpaces" | "keptMessages" | "keptContacts"> = { space: "keptSpaces", message: "keptMessages", contact: "keptContacts" }
  const keptBy = Object.entries(plan.kept.reduce<Record<string, number>>((m, k) => ({ ...m, [k.kind]: (m[k.kind] ?? 0) + 1 }), {})).map(([k, count]) => t(`integrations:cleanup.${KIND[k] ?? "keptItems"}`, { count }))
  const reasons = [...new Set(plan.kept.map((k) => k.reason))]
  const head = gone.length && (plan.messages || plan.spaces || plan.contacts || plan.messages_purged || plan.decisions) ? t("integrations:cleanup.willDelete", { list: gone.join(sep) }) : t("integrations:cleanup.nothing")
  const kept = plan.kept.length === plan.kept_count && keptBy.length ? keptBy.join(sep) : t("integrations:cleanup.keptItems", { count: plan.kept_count })
  const sentence = plan.kept_count <= 0 ? t("integrations:cleanup.sentence", { head })
    : reasons.length ? t("integrations:cleanup.sentenceKeptWhy", { head, kept, reasons: reasons.slice(0, 2).join(sep) }) : t("integrations:cleanup.sentenceKept", { head, kept })
  return (
    <div className="flex flex-col gap-2 rounded-xl bg-muted/60 px-4 py-3 type-callout" data-cleanup-plan data-plan-spaces={plan.spaces} data-plan-messages={plan.messages} data-plan-contacts={plan.contacts} data-plan-kept={plan.kept_count}>
      <div>{sentence}</div>
      {(plan.decisions ?? 0) > 0 && <div className="type-footnote text-muted-foreground" data-cleanup-undo-note>{t("integrations:cleanup.undoNote")}</div>}
      {plan.kept.length > 0 && <details className="group/disc type-footnote text-muted-foreground"><summary className={SUMMARY}><DiscChevron className="size-3" />{t("integrations:cleanup.showKept")}</summary>
        <ul className="mt-1 flex max-h-40 flex-col gap-0.5 overflow-y-auto">{plan.kept.map((k) => <li key={k.kind + k.id}>{k.name || k.id} · {k.reason}</li>)}</ul></details>}
    </div>
  )
}

function Choices({ value, onChange, allowKeep, offered }: { value: Choice; onChange: (v: Choice) => void; allowKeep: boolean; offered: boolean }) {
  const t = useT()
  const opts: Choice[] = allowKeep ? ["keep", "text", "all"] : ["text", "all"]
  return (
    <RadioGroup value={value} onValueChange={(v) => onChange(v as Choice)} className="gap-2" aria-label={t("integrations:cleanup.history")}>
      {opts.map((o) => (
        <label key={o} className={"flex cursor-pointer items-start gap-2.5 rounded-lg px-3 py-2.5 transition-[background-color,box-shadow] duration-180 ease-(--ease-out) " + (value === o ? "bg-primary/8 ring-1 ring-primary/40" : "bg-muted/60 hover:bg-accent")} data-cleanup-choice={o}>
          <RadioGroupItem value={o} disabled={o !== "keep" && !offered} className="mt-0.5" />
          <span className="min-w-0"><span className={"type-body " + (o === "all" ? "text-destructive" : "")}>{LEVEL_TEXT[o][0]}</span><span className="block type-footnote text-muted-foreground">{LEVEL_TEXT[o][1]}</span></span>
        </label>))}
      {!offered && <div className="type-footnote text-muted-foreground">{t("integrations:cleanup.notOffered")}</div>}
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
  const t = useT()
  const [usage, setUsage] = useState<RUsage[] | null>(null), [choice, setChoice] = useState<Choice>("keep"), [typed, setTyped] = useState(""), [busy, setBusy] = useState(false), [offered, setOffered] = useState(true)
  useEffect(() => { Promise.all(leaving.map((l) => accountsApi.usage(l.id))).then(setUsage, () => setUsage([])) }, [])
  useEffect(() => { cleanupApi.preview({ connection_id: leaving[0].id }, "text").catch((e) => { if (isNotWired(e)) setOffered(false) }) }, [])
  const targets = leaving.map((l) => ({ connection_id: l.id }))
  const plan = usePlan(targets, choice)
  const used = (usage ?? []).flatMap((u) => [...u.flows.map((f) => t("integrations:cleanup.usageFlow", { name: f.name })), ...u.sources.map((x) => t("integrations:cleanup.usageSource", { name: x.name }))])
  const spaces = (usage ?? []).reduce((n, u) => n + (u.spaces || 0), 0)
  const needName = choice === "all", ok = !needName || typed.trim() === name.trim()
  const go = async () => {
    setBusy(true)
    try {
      if (choice !== "keep") for (const [i, tg] of targets.entries()) await cleanupApi.run(tg, choice, plan?.previews?.[i]?.decision_ids)
      for (const l of [...leaving].reverse()) await accountsApi.disconnect(l.id)
      toast(choice === "keep" ? t("integrations:cleanup.disconnected") : t("integrations:cleanup.disconnectedCleaned")); onDone()
    } catch (e) { report(e) } finally { setBusy(false) }
  }
  return (
    <Dialog open onOpenChange={(o) => !o && !busy && onClose()}>
      <DialogContent className="max-h-[90dvh] gap-4 overflow-y-auto rounded-2xl" style={{ maxWidth: 520 }} data-disconnect-dialog>
        <DialogHeader><DialogTitle className="type-headline">{title}</DialogTitle>
          <DialogDescription className="type-callout">{t("integrations:cleanup.desc")}</DialogDescription></DialogHeader>
        <div className="flex flex-col gap-1.5 type-callout" data-disconnect-effects>
          <SectionLabel className="mb-0">{t("integrations:cleanup.stops")}</SectionLabel>
          <ul className="list-disc pl-5 text-muted-foreground">
            {leaving.map((l) => <li key={l.id}>{t("integrations:cleanup.stopLine", { label: l.label, stops: l.stops })}</li>)}
            {used.map((u) => <li key={u}>{u}</li>)}
          </ul>
          <SectionLabel className="mb-0 mt-2">{t("integrations:cleanup.keeps")}</SectionLabel>
          <ul className="list-disc pl-5 text-muted-foreground">
            <li>{spaces ? t("integrations:cleanup.keptChats", { count: spaces }) : t("integrations:cleanup.keptChatsAny")}</li>
            <li>{t("integrations:cleanup.keptRefs")}</li>
            {keptNote && <li>{keptNote}</li>}
          </ul>
        </div>
        <div className="flex flex-col gap-2"><SectionLabel className="mb-0">{t("integrations:cleanup.history")}</SectionLabel>
          <Choices value={choice} onChange={setChoice} allowKeep offered={offered} />
          {choice !== "keep" && (plan === undefined ? <div className="flex items-center gap-2 type-footnote text-muted-foreground"><Spinner className="size-3" />{t("integrations:cleanup.counting")}</div> : plan && <PlanSummary plan={plan} level={choice} />)}
          {needName && <label className="flex flex-col gap-1 type-footnote text-muted-foreground">{t("integrations:cleanup.typeToConfirm", { name })}<Input className="rounded-lg" value={typed} onChange={(e) => setTyped(e.target.value)} aria-label={t("integrations:cleanup.typeAria", { name })} data-cleanup-typed /></label>}
          {choice !== "keep" && <div className="type-footnote text-muted-foreground">{t("integrations:cleanup.backupFirst")}</div>}
        </div>
        <DialogFooter className="gap-2"><Button variant="outline" disabled={busy} onClick={onClose}>{t("common:action.cancel")}</Button>
          <Button variant="destructive" disabled={busy || !usage || !ok || (choice !== "keep" && !plan)} onClick={() => void go()} data-disconnect-confirm>{busy && <Spinner />}{choice === "keep" ? t("integrations:cleanup.disconnect") : t("integrations:cleanup.disconnectDelete")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/** 清理记录 without disconnecting (or for an identity already disconnected) */
export function CleanupDialog({ target, name, onClose, onDone }: { target: CleanupTarget; name: string; onClose: () => void; onDone: () => void }) {
  const t = useT()
  const [choice, setChoice] = useState<Choice>("text"), [typed, setTyped] = useState(""), [busy, setBusy] = useState(false)
  const plan = usePlan([target], choice)
  const level = choice as CleanupLevel, ok = level !== "all" || typed.trim() === name.trim()
  const empty = !!plan && !plan.messages && !plan.spaces && !plan.contacts && !plan.messages_purged && !plan.decisions
  const go = async () => {
    setBusy(true)
    try { const r = await cleanupApi.run(target, level, plan?.decision_ids); toast(r.backup ? t("integrations:cleanup.cleanedBackup") : t("integrations:cleanup.cleaned")); onDone() } catch (e) { report(e) } finally { setBusy(false) }
  }
  return (
    <Dialog open onOpenChange={(o) => !o && !busy && onClose()}>
      <DialogContent className="max-h-[90dvh] gap-4 overflow-y-auto rounded-2xl" style={{ maxWidth: 500 }} data-cleanup-dialog>
        <DialogHeader><DialogTitle className="type-headline">{t("integrations:cleanup.title", { name })}</DialogTitle>
          <DialogDescription className="type-callout">{t("integrations:cleanup.cleanupDesc")}</DialogDescription></DialogHeader>
        <Choices value={choice} onChange={setChoice} allowKeep={false} offered />
        {plan === undefined ? <div className="flex items-center gap-2 type-footnote text-muted-foreground"><Spinner className="size-3" />{t("integrations:cleanup.counting")}</div> : plan ? <PlanSummary plan={plan} level={level} /> : <div className="type-footnote text-muted-foreground">{t("integrations:cleanup.notOffered")}</div>}
        {level === "all" && <label className="flex flex-col gap-1 type-footnote text-muted-foreground">{t("integrations:cleanup.typeToConfirm", { name })}<Input className="rounded-lg" value={typed} onChange={(e) => setTyped(e.target.value)} aria-label={t("integrations:cleanup.typeAria", { name })} data-cleanup-typed /></label>}
        <DialogFooter className="gap-2"><Button variant="outline" disabled={busy} onClick={onClose}>{t("common:action.cancel")}</Button>
          <Button variant="destructive" disabled={busy || !plan || empty || !ok} onClick={() => void go()} data-cleanup-confirm>{busy && <Spinner />}{t("common:action.delete")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
