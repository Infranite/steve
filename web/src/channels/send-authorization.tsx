/* A recovery proposal uses the standard changeset card. Its receipt is separate from both
 * the current connection capabilities and the message's send state. Applying never retries. */
import { useCallback, useEffect, useRef, useState } from "react"
import { HubError, modReq, type Proposal } from "@/api"
import { accountsApi, type RConnection } from "@/api/accounts"
import { sendAuthorization } from "@/api/channels"
import { ProposalCard } from "@/config-ui"
import { Button } from "@/components/ui/button"
import { currentHub } from "@/hub"
import { useT } from "@/i18n"
import { emit, refresh, st, useHubEvents, type Wait } from "@/store"

type Receipt = { proposal: Proposal; phase: "draft" | "applied" | "undone" | "dropped" | "stale" | "uncertain"; kept?: string[] }
const CAP = "msg.send_as_user"

export function SendAuthorization({ w, locked, revision }: { w: Wait; locked: boolean; revision: number }) {
  const t = useT(), connection = w.recovery!.connection_id!, key = "decision:" + w.decision
  // Keep the actual proposal/acknowledgement across a page reload, scoped to this Hub, workspace and message.
  // This is only a receipt, never evidence of the current permission; always GET the connection again.
  const storageKey = JSON.stringify(["send-authorization", currentHub(), st.workspace?.id, w.subjectId])
  const [receipt, setReceipt] = useState<Receipt | null>(() => {
    try {
      const r = JSON.parse(sessionStorage.getItem(storageKey) ?? "null") as Receipt | null
      return r?.proposal?.target.kind === "channel_send_permission" && r.proposal.target.id === connection ? r : null
    } catch { return null }
  })
  const [current, setCurrent] = useState<RConnection | null>(null), [reading, setReading] = useState(true)
  const [error, setError] = useState(""), [readError, setReadError] = useState(false)
  const generation = useRef(0), active = useRef(true), running = useRef(false)
  const remember = (r: Receipt) => {
    if (!active.current) return
    setReceipt(r)
    try { sessionStorage.setItem(storageKey, JSON.stringify(r)) } catch { /* private mode: keep the receipt in this view */ }
  }
  const read = useCallback(async () => {
    const turn = ++generation.current
    setReading(true); setReadError(false)
    try {
      const c = await accountsApi.connection(connection)
      if (active.current && turn === generation.current) setCurrent(c)
    } catch {
      if (active.current && turn === generation.current) { setCurrent(null); setReadError(true) }
    } finally { if (active.current && turn === generation.current) setReading(false) }
  }, [connection])
  useEffect(() => { active.current = true; void read(); return () => { active.current = false; ++generation.current } }, [read, revision])
  useHubEvents(["connection"], () => void read(), 100, { filter: (e) => "id" in e && e.id === connection })

  const run = async (action: "create" | "apply" | "undo" | "drop") => {
    if (running.current || locked || st.pendingActions.has(key) || st.submittedChannelDecisions.has(w.decision) || st.inactiveChannelDecisions.has(w.decision)) return
    running.current = true; st.pendingActions.add(key); emit(); setError("")
    try {
      if (action === "create") {
        const proposal = await sendAuthorization(w.subjectId!)
        remember({ proposal, phase: "draft" })
      } else if (receipt) {
        const r = await modReq<{ ok: boolean; kept?: string[] }>("POST", `/config/${encodeURIComponent(receipt.proposal.id)}/${action}`, {})
        if (!r.ok) throw new Error(t("core:sendRecovery.authorizationUnconfirmed"))
        remember({ ...receipt, phase: action === "apply" ? "applied" : action === "undo" ? "undone" : "dropped", kept: r.kept })
      }
    } catch (e) {
      if (active.current) {
        setError((e as Error).message)
        // A conflict is definitive. A lost response could already have applied: do not repeat the mutation.
        if (receipt && action !== "create") remember({ ...receipt, phase: e instanceof HubError && e.status < 500 ? (e.status === 409 ? "stale" : receipt.phase) : "uncertain" })
      }
    } finally {
      await Promise.all([read(), refresh()])
      running.current = false; st.pendingActions.delete(key); emit()
    }
  }
  const phase = receipt?.phase, closed = phase && !["draft", "applied"].includes(phase)
  const enabled = current?.caps.includes(CAP), unavailable = current && !current.available_caps.includes(CAP)
  const disabled = locked || reading || !current
  const summary = phase && phase !== "draft" ? t(`core:sendRecovery.authorization.${phase}`) : ""
  return <section className="flex min-w-0 flex-col gap-2 [overflow-wrap:anywhere]" aria-label={t("core:sendRecovery.authorizationTitle")} data-send-authorization>
    <p>{t("core:sendRecovery.authorizationNote")}</p>
    <div role="status" aria-live="polite" data-authorization-current={current ? (enabled ? "enabled" : "disabled") : "unread"}>
      {t("core:sendRecovery.localCapability", { state: t(reading ? "core:sendRecovery.refreshing" : current ? (enabled ? "core:sendRecovery.enabled" : "core:sendRecovery.disabled") : "core:sendRecovery.unread") })}
    </div>
    {readError && <p className="text-attention" data-authorization-read-error>{t("core:sendRecovery.authorizationReadFailed")}</p>}
    {unavailable && <p className="text-attention">{t("core:sendRecovery.platformMissing")}</p>}
    {receipt && <p data-authorization-target>{t("core:sendRecovery.authorizationScope")} <code>{receipt.proposal.target.kind} / {receipt.proposal.target.id}</code></p>}
    {receipt && <ProposalCard p={{ ...receipt.proposal, state: phase === "applied" ? "applied" : "draft", apply: () => run("apply"), undo: () => run("undo"), drop: () => run("drop") }} disabled={disabled} settled={closed ? summary : undefined} />}
    {summary && <p role="status" data-authorization-result={phase}>{summary}</p>}
    {receipt?.kept?.length ? <p className="text-attention" data-authorization-kept>{t("core:sendRecovery.authorizationKept", { fields: receipt.kept.join(t("core:toast.fieldSep")) })}</p> : null}
    {error && <p role="alert" className="text-attention whitespace-pre-wrap" data-authorization-error>{error}</p>}
    {phase === "uncertain" && <Button size="sm" variant="outline" className="self-start" disabled={disabled} onClick={() => void run("undo")} data-authorization-undo-unknown>{t("core:sendRecovery.authorizationUndoUnknown")}</Button>}
    {(!receipt || closed) && <Button size="sm" variant="outline" className="self-start" disabled={disabled || !!enabled || !!unavailable} onClick={() => void run("create")} data-authorization-create>{t(receipt ? "core:sendRecovery.authorizationAgain" : "core:sendRecovery.authorizationTitle")}</Button>}
  </section>
}
