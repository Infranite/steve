/* The draft card (email.md §11): the same body in 待处理 (a `mail_draft` approve decision, rendered inside ui.tsx's
 * DecisionCard) and on the thread page. Recipients (外部 marked), subject, an editable body, attachments, the disclosure
 * basis; “发送” / “不发”. A changed body goes with the decision as `text` (your own edit). Once approved the mail waits
 * the mailbox's send delay; the toast's 撤销 calls `POST /mail/drafts/:id/cancel` and the draft is back to pending.
 * Nothing is sent from here except through that decision. */
import { useEffect, useState, type ReactNode } from "react"
import { ChevronDownIcon, ShieldAlertIcon, ShieldCheckIcon, ShieldQuestionIcon, Undo2Icon } from "lucide-react"
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

const EDITABLE = new Set(["draft", "pending"])

function Rcpts({ label, xs }: { label: string; xs: Rcpt[] }) {
  if (!xs.length) return null
  return (
    <div className="flex min-w-0 items-baseline gap-2 type-callout" data-rcpts={label}>
      <span className="w-12 shrink-0 text-muted-foreground">{label}</span>
      <span className="flex min-w-0 flex-wrap gap-1.5">{xs.map((r) => (
        <span key={r.addr} className="inline-flex max-w-full items-center gap-1 rounded-full bg-muted px-2.5 py-0.5 type-footnote" title={r.addr} data-rcpt={r.addr}>
          <span className="truncate">{r.name || r.addr}</span>{r.name && <span className="hidden truncate text-muted-foreground sm:inline">{r.addr}</span>}
          {r.external && <Badge variant="outline" className="ml-0.5 h-4 px-1 text-[10.5px] font-normal text-attention" data-external>外部</Badge>}
        </span>))}</span>
    </div>
  )
}

/** the disclosure check (§5.3): folded; flagged spans listed, nothing rewritten */
function Basis({ b }: { b: Draft["basis"] }) {
  const [open, setOpen] = useState(false)
  const v = b?.verdict ?? "unchecked", spans = b?.spans ?? []
  const [I, t, cls] = v === "unchecked" ? [ShieldQuestionIcon, "没有做披露检查", "text-muted-foreground"] : spans.length || v === "warn" || v === "block" ? [ShieldAlertIcon, `可能泄露 ${spans.length || ""} 处`.replace("  ", " "), "text-attention"] : [ShieldCheckIcon, "披露检查通过", "text-muted-foreground"]
  return (
    <div className="type-footnote" data-basis={v}>
      <button type="button" className={cn("inline-flex items-center gap-1", cls, spans.length && "hover:text-foreground")} aria-expanded={open} disabled={!spans.length} onClick={() => setOpen(!open)}>
        <I className="size-3.5" aria-hidden />{t}{spans.length > 0 && <ChevronDownIcon className={cn("size-3.5 transition-transform", open && "rotate-180")} aria-hidden />}</button>
      {open && <ul className="mt-1.5 flex flex-col gap-1 pl-5">{spans.map((s, n) => <li key={n} className="list-disc"><mark className="rounded bg-attention/15 px-0.5 text-foreground">{s.text}</mark>{s.why && <span className="text-muted-foreground"> · {s.why}</span>}</li>)}</ul>}
    </div>
  )
}

const secsLeft = (d: Draft) => (d.send_at ? Math.max(0, Math.round((d.send_at - Date.now()) / 1000)) : 0)

/** send / undo / discard for one draft; `send` goes through its decision (or opens one as you for your own draft) */
function useDraft(d0: Draft, onChange?: () => void) {
  const [d, setD] = useState(d0), [body, setBody] = useState(d0.body), [busy, setBusy] = useState(false)
  useEffect(() => { setD(d0); setBody((b) => (b === d.body ? d0.body : b)) }, [d0]) // a reload keeps an edit in progress
  const dirty = body !== d.body
  const run = async <R,>(f: () => Promise<R>): Promise<R | undefined> => { setBusy(true); try { return await f() } catch (e) { report(e); return undefined } finally { setBusy(false) } }
  const after = async () => { await refresh(); onChange?.() }
  const cancel = async (id: string) => {
    const r = await run(() => mailApi.cancelDraft(id))
    if (r) { setD(r); setBody(r.body); toast("已撤销，邮件不会发出；草稿回到等你批准"); await after() }
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
    if (r.status === "approved" && s > 0) toast(`将在 ${s} 秒后发出`, { duration: s * 1000, action: { label: "撤销", onClick: () => void cancel(r.id) } })
    else toast(r.status === "sent" ? "邮件已发出" : "已批准发送")
    await after()
  }
  const no = async (why: string) => {
    const ok = await run(async () => {
      if (d.status === "pending" && d.decision_id) await api.decide(d.decision_id, "no", why || undefined)
      else await mailApi.deleteDraft(d.id)
      return true
    })
    if (ok) { toast("不发了，草稿已放弃"); await after() }
  }
  const save = async () => { const r = await run(() => mailApi.putDraft(d.id, { body })); if (r) { setD(r); setBody(r.body); toast("草稿已保存") } }
  return { d, body, setBody, dirty, busy, send, no, save, cancel }
}

function Fields({ x, readOnly }: { x: ReturnType<typeof useDraft>; readOnly: boolean }) {
  const { d } = x
  return (
    <div className="flex flex-col gap-2.5" data-mail-draft={d.id} data-draft-status={d.status}>
      <Rcpts label="收件人" xs={d.to} /><Rcpts label="抄送" xs={d.cc} /><Rcpts label="密送" xs={d.bcc} />
      <div className="flex items-baseline gap-2 type-callout"><span className="w-12 shrink-0 text-muted-foreground">主题</span><span className="min-w-0 break-words">{d.subject}</span></div>
      {readOnly
        ? <div className="max-h-72 overflow-auto whitespace-pre-wrap break-words rounded-lg bg-muted/50 px-3 py-2 type-body" data-draft-body-ro>{d.body}</div>
        : <Textarea aria-label="正文" className="min-h-40 rounded-lg type-body" value={x.body} onChange={(e) => x.setBody(e.target.value)} maxLength={100_000} data-draft-body />}
      {x.dirty && !readOnly && <div className="type-footnote text-muted-foreground">你改过正文，发出的是你改过的版本。</div>}
      {d.files.length > 0 && <Files files={d.files} />}
      <Basis b={d.basis} />
      {d.error && <div className="type-footnote text-destructive" role="alert">{d.error}</div>}
    </div>
  )
}

function Buttons({ x, rework, setRework, why }: { x: ReturnType<typeof useDraft>; rework: boolean; setRework: (v: boolean) => void; why: string }) {
  const { d, busy } = x, s = secsLeft(d)
  if (d.status === "approved") return <><span className="mr-auto type-footnote text-muted-foreground" role="status">{s > 0 ? `将在 ${s} 秒后发出` : "正在发出"}</span>
    {s > 0 && <Button variant="outline" disabled={busy} onClick={() => void x.cancel(d.id)} data-draft-cancel><Undo2Icon />撤销</Button>}</>
  if (!EDITABLE.has(d.status)) return <span className="mr-auto inline-flex items-center gap-1.5 type-footnote text-muted-foreground"><StatusDot s={DRAFT_STATUS[d.status]?.[1] ?? "idle"} />{DRAFT_STATUS[d.status]?.[0] ?? d.status}</span>
  if (rework) return <><Button variant="ghost" onClick={() => setRework(false)}>取消</Button><Button variant="outline" disabled={busy} onClick={() => void x.no(why.trim())} data-draft-no-go>不发</Button></>
  return <>
    <Button variant="ghost" className="mr-auto text-muted-foreground" disabled={busy} onClick={() => setRework(true)} data-draft-no>不发…</Button>
    {d.status === "draft" && x.dirty && <Button variant="outline" disabled={busy} onClick={() => void x.save()} data-draft-save>保存</Button>}
    <Button disabled={busy || !x.body.trim()} onClick={() => void x.send()} data-draft-send>{busy && <Spinner />}发送</Button>
  </>
}

function useTick(on: boolean) { const [, set] = useState(0); useEffect(() => { if (!on) return; const t = setInterval(() => set((n) => n + 1), 1000); return () => clearInterval(t) }, [on]) }

function DraftParts({ d0, onChange, children }: { d0: Draft; onChange?: () => void; children: (p: { fields: ReactNode; buttons: ReactNode; x: ReturnType<typeof useDraft> }) => ReactNode }) {
  const x = useDraft(d0, onChange), [rework, setRework] = useState(false), [why, setWhy] = useState("")
  useTick(x.d.status === "approved")
  const fields = <>
    <Fields x={x} readOnly={!EDITABLE.has(x.d.status)} />
    {rework && <Textarea autoFocus className="mt-3 min-h-16 rounded-lg" placeholder="为什么不发？有会话时会转告起草的 Bot（可选）" value={why} onChange={(e) => setWhy(e.target.value)} data-draft-why />}
  </>
  return <>{children({ fields, buttons: <Buttons x={x} rework={rework} setRework={setRework} why={why} />, x })}</>
}

/* ───────── 待处理: inside DecisionCard ───────── */
type P = { w: Wait; act: (c: string, t?: string) => Promise<void>; busy: boolean; head: (t: ReactNode) => ReactNode; foot: (c: ReactNode) => ReactNode }
export function MailDraftCard({ w, head, foot, act, busy }: P) {
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
    {err && foot(<>{w.options.map((choice) => <Button key={choice} variant="outline" disabled={busy} onClick={() => void act(choice)}>{{ no: "不发", send: "发送", retry: "重试", discard: "放弃待办", confirmed_not_sent: "已核实未发送，重试" }[choice] ?? choice}</Button>)}</>)}
  </>
  const reconcile = d.status === "unknown"
  const failureButtons = (d.status === "failed" || reconcile) && <>
    <Button variant="ghost" className="mr-auto" disabled={busy} onClick={() => void act("discard")}>放弃待办</Button>
    <Button variant="outline" disabled={busy} onClick={() => void act(reconcile ? "confirmed_not_sent" : "retry")}>{reconcile ? "已核实未发送，重试" : "重试"}</Button>
  </>
  return <DraftParts d0={d}>{({ fields, buttons }) => <>{head(w.q)}<CardContent className="pb-4">{fields}{reconcile && <p className="mt-2 type-footnote text-attention">邮件可能已经发出。请先核实，重试可能造成重复发送；放弃待办不会撤回邮件。</p>}</CardContent>{foot(failureButtons || buttons)}</>}</DraftParts>
}

/* ───────── on the thread page ───────── */
export function ThreadDraft({ d, onChange }: { d: Draft; onChange: () => void }) {
  return (
    <DraftParts d0={d} onChange={onChange}>{({ fields, buttons, x }) => (
      <Card className="gap-0 rounded-xl py-0" data-thread-draft={d.id}>
        <CardHeader className="pt-3.5">
          <CardDescription className="flex items-center gap-2 type-footnote"><StatusDot s={DRAFT_STATUS[x.d.status]?.[1] ?? "idle"} /><span className="font-medium text-foreground/80">{DRAFT_STATUS[x.d.status]?.[0] ?? x.d.status}</span>
            <span className="text-tertiary">·</span><span>{x.d.author === "me" ? "你写的" : `${botName(x.d.author)} 起草`}</span></CardDescription>
          <CardTitle className="sr-only">草稿：{x.d.subject}</CardTitle>
        </CardHeader>
        <CardContent className="pb-4 pt-3">{fields}</CardContent>
        <CardFooter className="flex-wrap justify-end gap-2 border-t px-3 py-2.5!">{buttons}</CardFooter>
      </Card>)}
    </DraftParts>
  )
}
