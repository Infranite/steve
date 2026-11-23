/* A failed send stays reviewable. Read the existing context and paginated channel record; never infer provider permissions from an error string. */
import { useCallback, useEffect, useId, useRef, useState } from "react"
import { outContext, sendAuthorizationUrl, type ROutContext } from "@/api/channels"
import { conversationsApi, type ConvRecord, type RecordPage } from "@/api/conversations"
import { Button } from "@/components/ui/button"
import { Textarea } from "@/components/ui/textarea"
import { useT } from "@/i18n"
import { date } from "@/i18n/format"
import { decide, emit, st, useHubEvents, type Item, type Wait } from "@/store"
import { readViews } from "@/data/sync"
import { OutContext } from "./out-context"
import { MessageOpenInFeishu, OpenInFeishu } from "./open-in-app"
import { SendAuthorization } from "./send-authorization"

export function SendRecovery({ i, w }: { i: Item; w: Wait }) {
  const t = useT(), fieldId = useId(), submitting = st.pendingActions.has("decision:" + w.decision)
  const submitted = st.submittedChannelDecisions.has(w.decision), busy = submitting || submitted
  const [context, setContext] = useState<ROutContext | null>(null)
  const [record, setRecord] = useState<ConvRecord | null>(null)
  const [loading, setLoading] = useState(false), [failed, setFailed] = useState(false)
  const [editing, setEditing] = useState(false), [text, setText] = useState("")
  const [revision, setRevision] = useState(0)
  const generation = useRef(0)
  const load = useCallback(async () => {
    const turn = ++generation.current
    setLoading(true); setFailed(false)
    try {
      const c = w.subjectId ? await outContext(w.subjectId) : undefined
      if (turn !== generation.current) return
      setContext(c ?? null)
      let found: ConvRecord | null = null, cursor: RecordPage["next"] = null
      // A failure card may outlive the most recent page. Follow the server's cursor, never another message's state.
      const seen = new Set<string>()
      while (c) {
        const page = await conversationsApi.record("chan:" + c.space.id, cursor)
        if (turn !== generation.current) return
        found = page?.records.find((m) => m.id === w.subjectId) ?? null
        if (found || !page?.has_more || !page.next) break
        const key = JSON.stringify(page.next)
        if (seen.has(key)) break
        seen.add(key); cursor = page.next
      }
      if (turn === generation.current) {
        setRecord(found)
        if (found) {
          const inactive = inactiveRecord(found, w.decision)
          if (inactive !== st.inactiveChannelDecisions.has(w.decision)) {
            if (inactive) st.inactiveChannelDecisions.add(w.decision)
            else st.inactiveChannelDecisions.delete(w.decision)
            emit()
          }
        }
      }
    } catch {
      if (turn === generation.current) { setRecord(null); setFailed(true) }
    } finally { if (turn === generation.current) setLoading(false) }
  }, [w.subjectId, w.decision])
  useEffect(() => { void load(); return () => { ++generation.current } }, [load, submitting])
  useHubEvents(["chan_message"], () => void load(), 100, { filter: (e) => "id" in e && e.id === w.subjectId })

  const body = record?.text ?? context?.text ?? w.draft
  const status = record?.out?.status ?? record?.status
  const state = status === "failed" ? t("core:sendRecovery.failed") : status === "unknown" ? t("core:sendRecovery.unknown")
    : status === "pending" ? t("core:sendRecovery.pending") : status === "sent" ? t("core:sendRecovery.sent")
    : status === "draft" ? t("core:sendRecovery.draft") : status === "discarded" ? t("core:sendRecovery.discarded")
    : status === "recalled" ? t("core:sendRecovery.recalled") : t("core:sendRecovery.unread")
  const stale = !!record && inactiveRecord(record, w.decision)
  const locked = busy || loading || stale || st.inactiveChannelDecisions.has(w.decision)
  const recovery = w.recovery, reason = recovery?.error || record?.out?.error || w.q
  const managed = recovery?.authorization_kind === "managed" && recovery.kind === "missing_permission" && !!recovery.connection_id
    && !!w.subjectId && recovery.authorization_url === sendAuthorizationUrl(w.subjectId)
  const settings = recovery?.settings_href?.startsWith("#/channels/acc/") ? recovery.settings_href : null
  return <div className="flex min-w-0 flex-col gap-3" data-send-recovery>
    <OutContext context={context} />
    <section className="min-w-0" aria-label={t("core:sendRecovery.body")}>
      <div className="mb-1 font-medium text-foreground">{t("core:sendRecovery.body")}</div>
      {body != null && body !== "" ? <pre className="m-0 whitespace-pre-wrap font-sans [overflow-wrap:anywhere] text-foreground" data-send-body>{body}</pre>
        : <p data-send-body-missing>{t(loading ? "core:sendRecovery.loadingBody" : "core:sendRecovery.missingBody")}</p>}
    </section>
    <section className="min-w-0 rounded-lg bg-muted/60 p-3 [overflow-wrap:anywhere]" aria-label={t("core:sendRecovery.block")} data-send-block>
      <div className="font-medium text-foreground">{t("core:sendRecovery.block")}</div>
      <p className="whitespace-pre-wrap" data-send-reason>{reason}</p>
      <p className="mt-1! type-footnote text-muted-foreground" data-send-source>{recovery ? <>{t("core:sendRecovery.ruleSource")} <code>{recovery.source}</code></> : <>{t(record?.out?.error ? "core:sendRecovery.recordSource" : "core:sendRecovery.decisionSource")} <code>{record?.out?.error ? w.subjectId : w.decision}</code></>}</p>
      {recovery && <dl className="mt-2! grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 type-footnote" data-send-recovery-info>
        {([["kind", recovery.kind], ["scope", recovery.scope], ["authorizationKind", recovery.authorization_kind], ["provider", recovery.provider], ["code", recovery.code], ["rpcCode", recovery.rpc_code], ["connection", recovery.connection_id], ["space", recovery.space_id]] as const).map(([label, value]) => value != null && <div key={label} className="contents"><dt>{t(`core:sendRecovery.fields.${label}`)}</dt><dd className="min-w-0 whitespace-pre-wrap">{String(value)}</dd></div>)}
      </dl>}
      {recovery?.instructions && <p className="mt-2! whitespace-pre-wrap" data-send-instructions>{recovery.instructions}</p>}
      {!managed && <p className="mt-2!">{t("core:sendRecovery.restore")}</p>}
      {recovery?.authorization_kind === "external" && recovery.external_policy_known === false && <p className="mt-2!">{t("core:sendRecovery.externalUnknown")}</p>}
      {settings && <a className="mt-2 inline-block text-foreground underline" href={settings} data-send-settings>{t("core:sendRecovery.openSettings")}</a>}
    </section>
    {managed && <SendAuthorization key={w.subjectId} w={w} locked={locked} revision={revision} />}
    <div className="flex min-w-0 flex-wrap items-center gap-2" role="status" aria-live="polite" data-send-status={status ?? "unread"}>
      <span>{t("core:sendRecovery.state", { state })}</span>
      <Button size="xs" variant="outline" disabled={loading || submitting} onClick={() => { setRevision((n) => n + 1); void load(); void readViews("decision", [w.decision]).catch(() => {}) }} data-send-refresh>{t(loading ? "core:sendRecovery.refreshing" : "core:sendRecovery.refresh")}</Button>
      {busy && <p className="w-full" data-send-submitting>{t(submitted ? "core:sendRecovery.submitted" : "core:card.submitting")}</p>}
      {failed && <span className="w-full text-attention" data-send-read-error>{t("core:sendRecovery.readFailed")}</span>}
      {status === "unknown" && <p className="w-full text-attention">{t("core:sendRecovery.unknownNote")}</p>}
      {status === "sent" && <p className="w-full" data-send-receipt>{record?.sent ? t("core:sendRecovery.sentAt", { at: date(record.sent, { dateStyle: "short", timeStyle: "medium" }) }) : t("core:sendRecovery.markedSent")} {t("core:sendRecovery.noReceipt")}</p>}
      {stale && <p className="w-full" data-send-stale>{t("core:sendRecovery.changed")}</p>}
    </div>
    {record?.open_url ? <OpenInFeishu url={record.open_url} className="self-start" /> : <MessageOpenInFeishu message={w.subjectId} className="self-start" />}
    {w.options.includes("rewrite") && (editing ? <div className="flex min-w-0 flex-col gap-2" data-send-editor>
      <label htmlFor={fieldId} className="font-medium text-foreground">{t("core:sendRecovery.edit")}</label>
      <Textarea id={fieldId} value={text} onChange={(e) => setText(e.target.value)} disabled={locked} rows={4} maxRows={12} data-send-edit />
      <p>{t("core:sendRecovery.reviewNote")}</p>
      <div className="flex flex-wrap gap-2">
        <Button size="sm" disabled={locked || !text.trim()} onClick={() => void decide(i.id, "rewrite", text, w.decision)} data-send-review>{t("core:sendRecovery.review")}</Button>
        <Button size="sm" variant="ghost" disabled={busy} onClick={() => setEditing(false)} data-send-cancel-edit>{t("core:sendRecovery.cancelEdit")}</Button>
      </div>
    </div> : <Button size="sm" variant="outline" className="self-start" disabled={locked || body == null} onClick={() => { setText(body ?? ""); setEditing(true) }} data-fail="rewrite">{t("core:sendRecovery.edit")}</Button>)}
  </div>
}

const inactiveRecord = (m: ConvRecord, decision: string) => !["failed", "unknown"].includes(m.out?.status ?? m.status)
  || (m.out != null && m.out.decision !== decision)
