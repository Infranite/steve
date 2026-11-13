/* The draft card (email.md §11): the same body in 待处理 (a `mail_draft` approve decision, rendered inside ui.tsx's
 * DecisionCard) and on the thread page. Recipients (外部 marked), subject, an editable body, attachments, the disclosure
 * basis; “发送” / “不发”. A changed body goes with the decision as `text` (your own edit). Once approved the mail waits
 * the mailbox's send delay; the toast's 撤销 calls `POST /mail/drafts/:id/cancel` and the draft is back to pending.
 * Nothing is sent from here except through that decision. */
import { useEffect, useState, type ReactNode } from "react"
import { ChevronDownIcon, ShieldAlertIcon, ShieldCheckIcon, ShieldQuestionIcon, Undo2Icon } from "lucide-react"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { cn } from "cn"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Card, CardContent, CardFooter, CardHeader, CardTitle, CardDescription } from "@/components/ui/card"
import { Textarea } from "@/components/ui/textarea"
import { Spinner } from "@/components/ui/spinner"
import { api } from "@/api"
import { refresh, toast, botName, type Wait } from "@/store"
import { StatusDot } from "@/ui"
import { Files } from "@/attach"
import { report } from "@/accounts/load"
import { mailApi, DRAFT_STATUS, type Draft, type Rcpt } from "@/api/mail"
import { isNotWired } from "@/api"
import { useT } from "@/i18n"

const EDITABLE = new Set(["draft", "pending"])

function Rcpts({ kind, xs }: { kind: "to" | "cc" | "bcc"; xs: Rcpt[] }) {
  const t = useT()
  if (!xs.length) return null
  const label = t(`mail:draft.${kind}`)
  return (
    <div className="flex min-w-0 items-baseline gap-2 type-callout" data-rcpts={kind}>
      <span className="w-12 shrink-0 text-muted-foreground">{label}</span>
      <span className="flex min-w-0 flex-wrap gap-1.5">{xs.map((r) => (
        <span key={r.addr} className="inline-flex max-w-full items-center gap-1 rounded-full bg-muted px-2.5 py-0.5 type-footnote" title={r.addr} data-rcpt={r.addr}>
          <span className="truncate">{r.name || r.addr}</span>{r.name && <span className="hidden truncate text-muted-foreground sm:inline">{r.addr}</span>}
          {r.external && <Badge variant="secondary" className="ml-0.5 h-4 bg-attention/12 px-1.5 text-attention" data-external>{t("mail:draft.external")}</Badge>}
        </span>))}</span>
    </div>
  )
}

/** the disclosure check (§5.3): folded; flagged spans listed, nothing rewritten */
function Basis({ b, onCheck, busy }: { b: Draft["basis"]; onCheck?: () => void; busy?: boolean }) {
  const t = useT()
  const [open, setOpen] = useState(false)
  const v = b?.verdict ?? "unchecked", spans = b?.spans ?? []
  const [I, label, cls] = v === "unchecked" ? [ShieldQuestionIcon, t("mail:draft.unchecked"), "text-muted-foreground"]
    : spans.length || v === "warn" || v === "block" ? [ShieldAlertIcon, spans.length ? t("mail:draft.leak", { count: spans.length }) : t("mail:draft.leakUncounted"), "text-attention"]
    : [ShieldCheckIcon, t("mail:draft.passed"), "text-muted-foreground"]
  return (
    <Collapsible open={open} onOpenChange={setOpen} className="type-footnote" data-basis={v}>
      <CollapsibleTrigger className={cn("group/disc inline-flex items-center gap-1 rounded-md", cls, spans.length && "hover:text-foreground")} disabled={!spans.length}>
        <I className="size-3.5" aria-hidden />{label}{spans.length > 0 && <ChevronDownIcon className="size-3.5 transition-transform duration-180 ease-(--ease-out) group-aria-expanded/disc:rotate-180" aria-hidden />}</CollapsibleTrigger>
      {v === "unchecked" && onCheck && <Button size="xs" variant="ghost" className="ml-1.5 text-muted-foreground" disabled={busy} onClick={onCheck} data-draft-check>{t("mail:draft.check")}</Button>}
      <CollapsibleContent><ul className="flex flex-col gap-1 pl-5 pt-1.5">{spans.map((s, n) => <li key={n} className="list-disc"><mark className="rounded-md bg-attention/15 px-0.5 text-foreground">{s.text}</mark>{s.why && <span className="text-muted-foreground"> · {s.why}</span>}</li>)}</ul></CollapsibleContent>
    </Collapsible>
  )
}

const secsLeft = (d: Draft) => (d.send_at ? Math.max(0, Math.round((d.send_at - Date.now()) / 1000)) : 0)

/** send / undo / discard for one draft; `send` goes through its decision (or opens one as you for your own draft) */
function useDraft(d0: Draft, onChange?: () => void) {
  const t = useT()
  const [d, setD] = useState(d0), [body, setBody] = useState(d0.body), [busy, setBusy] = useState(false)
  useEffect(() => { setD(d0); setBody((b) => (b === d.body ? d0.body : b)) }, [d0]) // a reload keeps an edit in progress
  const dirty = body !== d.body
  const run = async <R,>(f: () => Promise<R>): Promise<R | undefined> => { setBusy(true); try { return await f() } catch (e) { report(e); return undefined } finally { setBusy(false) } }
  const after = async () => { await refresh(); onChange?.() }
  const cancel = async (id: string) => {
    const r = await run(() => mailApi.cancelDraft(id))
    if (r) { setD(r); setBody(r.body); toast(t("mail:draft.undone")); await after() }
  }
  const send = async () => {
    const r = await run(async () => {
      if (d.status === "pending" && d.decision_id) await api.decide(d.decision_id, "send", dirty ? body : undefined)
      else { if (dirty) await mailApi.putDraft(d.id, { body }); await mailApi.sendDraft(d.id) }
      return mailApi.draft(d.id)
    })
    if (!r) return
    setD(r); setBody(r.body)
    const s = secsLeft(r)
    if (r.status === "approved" && s > 0) toast(t("mail:draft.sendingIn", { count: s }), { duration: s * 1000, action: { label: t("mail:draft.undo"), onClick: () => void cancel(r.id) } })
    else toast(r.status === "sent" ? t("mail:draft.sent") : t("mail:draft.approved"))
    await after()
  }
  const no = async (why: string) => {
    const ok = await run(async () => {
      if (d.status === "pending" && d.decision_id) await api.decide(d.decision_id, "no", why || undefined)
      else await mailApi.deleteDraft(d.id)
      return true
    })
    if (ok) { toast(t("mail:draft.discarded")); await after() }
  }
  const save = async () => { const r = await run(() => mailApi.putDraft(d.id, { body })); if (r) { setD(r); setBody(r.body); toast(t("mail:draft.saved")) } }
  /* the check reads the stored draft: an edit in progress is saved first */
  const check = async () => { const r = await run(async () => { if (dirty) await mailApi.putDraft(d.id, { body }); return mailApi.checkDraft(d.id) }); if (r) { setD(r); setBody(r.body); toast(t("mail:draft.checked")) } }
  return { d, body, setBody, dirty, busy, send, no, save, cancel, check }
}

function Fields({ x, readOnly }: { x: ReturnType<typeof useDraft>; readOnly: boolean }) {
  const t = useT()
  const { d } = x
  return (
    <div className="flex flex-col gap-2.5" data-mail-draft={d.id} data-draft-status={d.status}>
      <Rcpts kind="to" xs={d.to} /><Rcpts kind="cc" xs={d.cc} /><Rcpts kind="bcc" xs={d.bcc} />
      <div className="flex items-baseline gap-2 type-callout"><span className="w-12 shrink-0 text-muted-foreground">{t("mail:draft.subject")}</span><span className="min-w-0 break-words">{d.subject}</span></div>
      {readOnly
        ? <div className="max-h-72 overflow-auto whitespace-pre-wrap break-words rounded-lg bg-muted/60 px-3.5 py-2.5 type-body" data-draft-body-ro>{d.body}</div>
        : <Textarea aria-label={t("mail:draft.body")} className="min-h-40 rounded-lg md:type-body" value={x.body} onChange={(e) => x.setBody(e.target.value)} maxLength={100_000} data-draft-body />}
      {x.dirty && !readOnly && <div className="type-footnote text-muted-foreground">{t("mail:draft.edited")}</div>}
      {d.files.length > 0 && <Files files={d.files} />}
      <Basis b={d.basis} busy={x.busy} onCheck={!readOnly && EDITABLE.has(d.status) ? () => void x.check() : undefined} />
      {d.error && <div className="type-footnote text-destructive" role="alert">{d.error}</div>}
    </div>
  )
}

function Buttons({ x, rework, setRework, why }: { x: ReturnType<typeof useDraft>; rework: boolean; setRework: (v: boolean) => void; why: string }) {
  const t = useT()
  const { d, busy } = x, s = secsLeft(d)
  if (d.status === "approved") return <><span className="mr-auto type-footnote text-muted-foreground" role="status">{s > 0 ? t("mail:draft.sendingIn", { count: s }) : t("mail:draft.sending")}</span>
    {s > 0 && <Button variant="outline" disabled={busy} onClick={() => void x.cancel(d.id)} data-draft-cancel><Undo2Icon />{t("mail:draft.undo")}</Button>}</>
  if (!EDITABLE.has(d.status)) return <span className="mr-auto inline-flex items-center gap-1.5 type-footnote text-muted-foreground"><StatusDot s={DRAFT_STATUS[d.status]?.[1] ?? "idle"} />{DRAFT_STATUS[d.status]?.[0] ?? d.status}</span>
  if (rework) return <><Button variant="ghost" onClick={() => setRework(false)}>{t("common:action.cancel")}</Button><Button variant="outline" disabled={busy} onClick={() => void x.no(why.trim())} data-draft-no-go>{t("mail:draft.no")}</Button></>
  return <>
    <Button variant="ghost" className="mr-auto text-muted-foreground" disabled={busy} onClick={() => setRework(true)} data-draft-no>{t("mail:draft.noEllipsis")}</Button>
    {d.status === "draft" && x.dirty && <Button variant="outline" disabled={busy} onClick={() => void x.save()} data-draft-save>{t("common:action.save")}</Button>}
    <Button disabled={busy || !x.body.trim()} onClick={() => void x.send()} data-draft-send>{busy && <Spinner />}{t("mail:draft.send")}</Button>
  </>
}

function useTick(on: boolean) { const [, set] = useState(0); useEffect(() => { if (!on) return; const t = setInterval(() => set((n) => n + 1), 1000); return () => clearInterval(t) }, [on]) }

function DraftParts({ d0, onChange, children }: { d0: Draft; onChange?: () => void; children: (p: { fields: ReactNode; buttons: ReactNode; x: ReturnType<typeof useDraft> }) => ReactNode }) {
  const t = useT()
  const x = useDraft(d0, onChange), [rework, setRework] = useState(false), [why, setWhy] = useState("")
  useTick(x.d.status === "approved")
  const fields = <>
    <Fields x={x} readOnly={!EDITABLE.has(x.d.status)} />
    {rework && <Textarea autoFocus className="mt-3 min-h-16 rounded-lg" placeholder={t("mail:draft.whyPlaceholder")} value={why} onChange={(e) => setWhy(e.target.value)} data-draft-why />}
  </>
  return <>{children({ fields, buttons: <Buttons x={x} rework={rework} setRework={setRework} why={why} />, x })}</>
}

/* ───────── 待处理: inside DecisionCard ───────── */
type P = { w: Wait; act: (c: string, t?: string) => Promise<void>; busy: boolean; head: (t: ReactNode) => ReactNode; foot: (c: ReactNode) => ReactNode }
export function MailDraftCard({ w, head, foot, act, busy }: P) {
  const t = useT()
  const [d, setD] = useState<Draft | null>(null), [err, setErr] = useState<"" | "unwired" | string>("")
  useEffect(() => {
    let live = true
    if (w.subjectId) mailApi.draft(w.subjectId).then((r) => live && setD(r), (e) => live && setErr(isNotWired(e) ? "unwired" : (e as Error).message))
    return () => { live = false }
  }, [w.subjectId, w.decision])
  /* without the draft (another Hub, or it's gone) the card still answers from the decision's own text */
  if (!d) return <>
    {head(w.q)}
    <CardContent className="pb-4">{err ? <><pre className="max-h-72 overflow-auto whitespace-pre-wrap break-words type-body text-muted-foreground">{w.draft}</pre>{err !== "unwired" && <div className="mt-2 type-footnote text-destructive">{err}</div>}</>
      : <div className="flex justify-center py-6"><Spinner className="text-muted-foreground" /></div>}</CardContent>
    {err && foot(<>{w.options.map((choice) => <Button key={choice} variant="outline" disabled={busy} onClick={() => void act(choice)}>{({ no: t("mail:draft.no"), send: t("mail:draft.send"), retry: t("common:action.retry"), discard: t("mail:draft.discardTodo"), confirmed_not_sent: t("mail:draft.confirmedNotSent") } as Record<string, string>)[choice] ?? choice}</Button>)}</>)}
  </>
  const reconcile = d.status === "unknown"
  const failureButtons = (d.status === "failed" || reconcile) && <>
    <Button variant="ghost" className="mr-auto" disabled={busy} onClick={() => void act("discard")}>{t("mail:draft.discardTodo")}</Button>
    <Button variant="outline" disabled={busy} onClick={() => void act(reconcile ? "confirmed_not_sent" : "retry")}>{reconcile ? t("mail:draft.confirmedNotSent") : t("common:action.retry")}</Button>
  </>
  return <DraftParts d0={d}>{({ fields, buttons }) => <>{head(w.q)}<CardContent className="pb-4">{fields}{reconcile && <p className="mt-2 type-footnote text-attention">{t("mail:draft.reconcileWarn")}</p>}</CardContent>{foot(failureButtons || buttons)}</>}</DraftParts>
}

/* ───────── on the thread page ───────── */
export function ThreadDraft({ d, onChange }: { d: Draft; onChange: () => void }) {
  const t = useT()
  return (
    <DraftParts d0={d} onChange={onChange}>{({ fields, buttons, x }) => (
      <Card className="gap-0 rounded-xl py-0 shadow-[var(--lift)] anim-in" data-thread-draft={d.id}>
        <CardHeader className="px-4 pt-4">
          <CardDescription className="flex items-center gap-2 type-footnote"><StatusDot s={DRAFT_STATUS[x.d.status]?.[1] ?? "idle"} /><span className="font-medium text-foreground/80">{DRAFT_STATUS[x.d.status]?.[0] ?? x.d.status}</span>
            <span className="text-tertiary">·</span><span>{x.d.author === "me" ? t("mail:draft.byMe") : t("mail:draft.byBot", { bot: botName(x.d.author) })}</span></CardDescription>
          <CardTitle className="sr-only">{t("mail:draft.srTitle", { subject: x.d.subject })}</CardTitle>
        </CardHeader>
        <CardContent className="px-4 pb-4 pt-3">{fields}</CardContent>
        <CardFooter className="flex-wrap justify-end gap-2 border-t px-3 py-2.5!">{buttons}</CardFooter>
      </Card>)}
    </DraftParts>
  )
}
