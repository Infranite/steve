/* A paired Node's two ways out, on its page: "Unpair" (it stays installed on its machine but can no longer connect) and
 * "Remove from this machine" (the Node itself stops, uninstalls its service and deletes its home, then is unpaired).
 * The removal asks for confirmation first and shows its steps while it runs; when the Hub cannot ask the Node (offline,
 * of an earlier release) or the Node did not confirm, it shows the command to run on that machine instead. What it shows
 * is devices-remove.ts's removeView; the dialog is affectUI's (components/ui/dialog), the command its Code Block. */
import { useEffect, useState } from "react"
import { InfoIcon, TriangleAlertIcon, Trash2Icon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { CodeBlock } from "@/components/ui/code-block"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { api } from "@/api"
import { toast } from "@/store"
import { useT } from "@/i18n"
import { span } from "@/i18n/format"
import { go } from "@/ui"
import type { NodeView } from "@/device-model"
import { useDevices } from "@/devices-data"
import { DCard, DRow } from "@/devices-ui"
import { REMOVE_STEPS, removeSteps, removeView, type RemoveView } from "@/devices-remove"
import { cn } from "cn"

const BACK = "#/devices/nodes"

/** the two rows on the Node's page, each saying what it does to the machine */
export function LeaveCard({ onUnpair, onRemove }: { onUnpair: () => void; onRemove: () => void }) {
  const t = useT()
  return <DCard data-node-unpair>
    <DRow title={t("devices:node.revoke")} sub={t("devices:node.revokeSub")}>
      <Button size="sm" variant="outline" onClick={onUnpair} data-node-unpair-open>{t("devices:node.revokeShort")}</Button>
    </DRow>
    <div className="border-t border-border/70"><DRow title={<span className="text-destructive">{t("devices:remove.action")}</span>} sub={t("devices:remove.sub")}>
      <Button size="sm" variant="outline" className="text-destructive" onClick={onRemove} data-node-remove-open><Trash2Icon />{t("devices:remove.short")}</Button>
    </DRow></div>
  </DCard>
}

function Command({ command }: { command: string }) {
  const t = useT()
  return <div className="flex min-w-0 flex-col gap-1.5" data-node-remove-command>
    <span className="type-callout text-muted-foreground">{t("devices:remove.runThere")}</span>
    <CodeBlock code={command} language="bash" />
    <span className="type-footnote text-muted-foreground">{t("devices:remove.commandNote")}</span>
  </div>
}

function Steps({ v }: { v: Extract<RemoveView, { kind: "running" }> }) {
  const t = useT()
  const states = removeSteps(v.step)
  return <ol className="flex flex-col gap-2" data-node-remove-steps>
    {REMOVE_STEPS.map((k, i) => <li key={k} className={cn("flex items-center gap-2.5 type-callout", states[i] === "todo" ? "text-tertiary" : states[i] === "active" ? "font-medium text-foreground" : "text-muted-foreground")} data-step={k} data-step-state={states[i]}>
      {states[i] === "active" ? <Spinner className="size-3.5" /> : <span className={cn("size-2 rounded-full", states[i] === "done" ? "bg-success" : "bg-muted")} aria-hidden />}
      <span>{t(`devices:remove.step.${k}`)}{k === "draining" && v.run.phase === "cancelling" ? ` · ${t("devices:remove.cancelling")}` : ""}</span>
    </li>)}
  </ol>
}

export function RemoveDialog({ v, onClose }: { v: NodeView; onClose: () => void }) {
  const t = useT(), { reloadNodes } = useDevices()
  const [busy, setBusy] = useState(false), [refused, setRefused] = useState<string | null>(null), [started, setStarted] = useState<string | null>(null)
  const view = removeView(v.node, refused, started)
  /* the removal this dialog started is through: the Node is unpaired, its page goes */
  const removed = view.kind === "removed" && !!started
  useEffect(() => {
    if (!removed) return
    toast(t("devices:remove.done", { name: v.name })); onClose(); go(BACK)
  }, [removed]) // eslint-disable-line react-hooks/exhaustive-deps
  const start = async () => {
    setBusy(true)
    try { const r = await api.removeNode(v.id); setStarted(r.operation); reloadNodes() } catch (e) {
      const why = (e as Error).message
      if (why === "offline" || why === "manual") setRefused(why)
      else toast(why === "busy" ? t("devices:remove.busy") : why)
    } finally { setBusy(false) }
  }
  const unpair = async () => {
    setBusy(true)
    try { await api.revokeNode(v.id); toast(t("devices:node.revoked", { name: v.name })); reloadNodes(); onClose(); go(BACK) } catch (e) { toast((e as Error).message) } finally { setBusy(false) }
  }
  const grace = view.kind === "ask" && view.grace ? span(view.grace * 1000) : "—"
  return <Dialog open onOpenChange={(x) => !x && onClose()}><DialogContent className="sm:max-w-lg" data-node-remove-dialog={view.kind}>
    <DialogHeader>
      <DialogTitle>{t("devices:remove.title", { name: v.name })}</DialogTitle>
      <DialogDescription>{view.kind === "manual" ? t(`devices:remove.why.${view.why}`) : view.kind === "running" ? t("devices:remove.runningSub") : view.kind === "ended" ? t("devices:remove.endedSub") : t("devices:remove.intro")}</DialogDescription>
    </DialogHeader>
    <div className="flex min-w-0 flex-col gap-4">
      {view.kind === "ask" && <>
        <ol className="flex list-decimal flex-col gap-1.5 pl-5 type-callout" data-node-remove-plan>
          <li>{t("devices:remove.plan.stop")}</li>
          <li>{t("devices:remove.plan.grace", { grace })}</li>
          <li>{t("devices:remove.plan.uninstall")}</li>
          <li>{t("devices:remove.plan.unpair")}</li>
        </ol>
        {view.last && <div className="flex gap-2.5 rounded-lg bg-attention/10 px-3.5 py-2.5 type-callout" data-node-remove-last>
          <InfoIcon className="mt-0.5 size-4 shrink-0 text-attention" aria-hidden />
          <span className="min-w-0">{t("devices:remove.lastFailed")}{view.last.error ? <code className="ml-1 type-code [overflow-wrap:anywhere]">{view.last.error}</code> : null}</span>
        </div>}
        <p className="type-footnote text-muted-foreground">{t("devices:remove.undo")}</p>
      </>}
      {view.kind === "running" && <Steps v={view} />}
      {view.kind === "ended" && <div className="flex gap-2.5 rounded-lg bg-destructive/8 px-3.5 py-3 type-callout" role="alert" data-node-remove-failed>
        <TriangleAlertIcon className="mt-0.5 size-4 shrink-0 text-destructive" aria-hidden />
        <span className="flex min-w-0 flex-col gap-1"><strong className="font-semibold text-foreground">{t(`devices:remove.ended.${view.run.phase === "failed" ? "failed" : "unconfirmed"}`)}</strong>
          {view.run.error && <code className="type-code whitespace-pre-wrap text-muted-foreground [overflow-wrap:anywhere]">{view.run.error}</code>}</span>
      </div>}
      {(view.kind === "manual" || view.kind === "ended") && <Command command={view.command} />}
    </div>
    <DialogFooter>
      <Button variant="outline" onClick={onClose}>{t(view.kind === "running" ? "common:action.close" : "common:action.cancel")}</Button>
      {(view.kind === "manual" || view.kind === "ended") && <Button variant="destructive" disabled={busy} onClick={() => void unpair()} data-node-remove-unpair>{t("devices:remove.unpairNow")}</Button>}
      {view.kind === "ask" && <Button variant="destructive" disabled={busy} onClick={() => void start()} data-node-remove-confirm>{busy ? <Spinner className="size-4" /> : <Trash2Icon />}{t("devices:remove.confirm")}</Button>}
    </DialogFooter>
  </DialogContent></Dialog>
}
