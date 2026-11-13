/* The queue strip under a composer: every message waiting for the session's next turn is its own row,
 * with 编辑 (inline), 撤回 (back into the composer when it is empty, else an undo toast) and 立即发送 (interrupts the
 * running turn; the queue, this message first, is the next turn). Attachments stay with the message.
 * While the session is still starting there is no turn to interrupt, so nothing offers 立即发送, and Steve's brief
 * (the session's first turn once it is up) is shown as what it is, without controls.
 * The main conversation has the same strip (ChatQueue): what you say while Steve is on a turn waits for it.
 * On an item page the queue is shown on the comments instead (QueuedComment); the strip keeps only what pairs with none. */
import { useState, type ReactNode } from "react"
import { ClockIcon, PencilIcon, RocketIcon, SendHorizontalIcon, Undo2Icon } from "lucide-react"
import { Spinner } from "@/components/ui/spinner"
import { Button } from "@/components/ui/button"
import { Textarea } from "@/components/ui/textarea"
import { Files } from "@/attach"
import { Markdown } from "@/components/markdown"
import type { RQueued } from "@/api"
import { chatBusy, qEdit, qSendNow, qWithdraw, st, threadQueuedEdit, threadQueuedWithdraw, type Sess } from "@/store"
import { Brief, type ComposerHandle } from "@/ui"
import { Trans } from "react-i18next"
import { t, useT } from "@/i18n"

/** when the queue goes, in the strip's header */
export function queueNote(s: Sess | null) {
  if (st.hold) return t("shell:queue.afterRelease")
  if (s?.starting) return t("shell:queue.afterStart")
  if (s?.st === "run") return t("shell:queue.afterTurn")
  return t("shell:queue.notRunning")
}

const snip = (s: string) => (s.length > 24 ? s.slice(0, 24) + "…" : s)

/** `prefix`: what the Hub put in front of the user's words (item supplements: “补充信息：”); shown and edited without it */
export function QueueStrip({ sid, rows, note, composer, prefix = "", starting, className }: {
  sid: string; rows: RQueued[]; note: string; composer?: { current: ComposerHandle | null }; prefix?: string; starting?: boolean; className?: string
}) {
  const t = useT()
  if (!rows.length) return null
  const strip = (s: string) => (prefix && s.startsWith(prefix) ? s.slice(prefix.length) : s)
  const brief = !!starting && rows.every((q) => q.via === "steve")
  return (
    <div role="region" aria-label={t("shell:queue.region")} className={"mb-2 rounded-xl bg-muted/60 anim-in " + (className ?? "")} data-sq>
      <div className="flex items-center gap-2 px-4 pt-2.5 pb-1 type-footnote text-muted-foreground" data-sq-note>
        <ClockIcon className="size-3.5 shrink-0" aria-hidden /><span>{brief ? t("shell:queue.briefWaiting") : t("shell:queue.queuedN", { count: rows.length, note })}</span>
      </div>
      <ul className="flex flex-col divide-y divide-foreground/6 pb-0.5">
        {rows.map((q) => <Row key={q.id} q={q} shown={strip(q.text)} ops={starting && q.via === "steve" ? "none" : {
          edit: (text) => qEdit(sid, q.id, (q.text.startsWith(prefix) ? prefix : "") + text),
          withdraw: () => qWithdraw(sid, q.id, composer?.current, strip),
          sendNow: starting ? undefined : () => qSendNow(sid, q.id),
        }} />)}
      </ul>
    </div>
  )
}

/** the main conversation's queue: messages said while Steve was on a turn, sent one by one once it ends */
export function ChatQueue({ composer }: { composer?: { current: ComposerHandle | null } }) {
  const rows = st.queue, t = useT()
  if (!rows.length) return null
  const note = st.hold ? t("shell:queue.afterRelease") : chatBusy() && (st.thinking || st.live) ? t("shell:queue.afterTurnInOrder") : t("shell:queue.now")
  return (
    <div role="region" aria-label={t("shell:queue.region")} className="mb-2 rounded-xl bg-muted/60 anim-in" data-sq data-chat-queue>
      <div className="flex items-center gap-2 px-4 pt-2.5 pb-1 type-footnote text-muted-foreground" data-sq-note>
        <ClockIcon className="size-3.5 shrink-0" aria-hidden /><span>{t("shell:queue.queuedN", { count: rows.length, note })}</span>
      </div>
      <ul className="flex flex-col divide-y divide-foreground/6 pb-0.5">
        {rows.map((m) => <Row key={m.id ?? m.clientId} q={{ id: m.id ?? "", text: m.t ?? "", files: m.files, via: "me" }} shown={m.t ?? ""} ops={m.id ? {
          edit: (text) => threadQueuedEdit(m.id!, text),
          withdraw: () => threadQueuedWithdraw(m.id!, composer?.current),
        } : null} />)}
      </ul>
    </div>
  )
}

type QRow = Pick<RQueued, "id" | "text" | "files" | "via">
/** what a row can do; `null` while it is still being sent, `"none"` when it only waits (Steve's brief before the session is up) */
type QOps = { edit: (text: string) => Promise<unknown>; withdraw: () => Promise<unknown>; sendNow?: () => Promise<unknown> } | "none" | null

/** inline editing of one queued message: `edit` is the draft while editing, else null */
function useQueuedEdit(shown: string, ops: QOps) {
  const [edit, setEdit] = useState<string | null>(null), [busy, setBusy] = useState(false)
  const run = async (f: () => Promise<unknown>) => { setBusy(true); try { await f() } finally { setBusy(false) } }
  const save = () => run(async () => {
    if (!ops || ops === "none") return
    const t = (edit ?? "").trim()
    if (!t || t === shown) { setEdit(null); return }
    await ops.edit(t)
    setEdit(null) // saved, or already sent (the row is gone after the refresh)
  })
  return { edit, setEdit, busy, run, save }
}
type QEdit = ReturnType<typeof useQueuedEdit>

function EditBox({ e }: { e: QEdit }) {
  const t = useT()
  return <Textarea autoFocus value={e.edit ?? ""} onChange={(x) => e.setEdit(x.target.value)} aria-label={t("shell:queue.editLabel")} className="min-h-14 bg-card type-body" data-queued-input
    onKeyDown={(x) => { if (x.key === "Escape") { x.preventDefault(); e.setEdit(null) } if (x.key === "Enter" && (x.metaKey || x.ctrlKey)) { x.preventDefault(); void e.save() } }} />
}

function Actions({ e, shown, ops, via, className }: { e: QEdit; shown: string; ops: Exclude<QOps, "none" | null>; via: string; className?: string }) {
  const { edit, busy, run } = e, t = useT()
  return <div className={"flex shrink-0 flex-wrap items-center gap-0.5 " + (className ?? "")}>
    {edit !== null ? <>
      <Button size="xs" onClick={() => void e.save()} disabled={busy || !edit.trim()} data-queued-save>{t("common:action.save")}</Button>
      <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => e.setEdit(null)} disabled={busy}>{t("common:action.cancel")}</Button>
    </> : <>
      {via !== "steve" && <Button size="xs" variant="ghost" className="text-muted-foreground" aria-label={t("shell:queue.editAria", { text: snip(shown) })} disabled={busy} onClick={() => e.setEdit(shown)} data-queued-edit><PencilIcon />{t("common:action.edit")}</Button>}
      <Button size="xs" variant="ghost" className="text-muted-foreground" aria-label={t("shell:queue.withdrawAria", { text: snip(shown) })} disabled={busy} onClick={() => void run(ops.withdraw)} data-queued-withdraw><Undo2Icon />{t("shell:queue.withdraw")}</Button>
      {ops.sendNow && <Button size="xs" variant="ghost" className="text-blue" aria-label={t("shell:queue.sendNowAria", { text: snip(shown) })} disabled={busy} onClick={() => void run(ops.sendNow!)} data-queued-send><SendHorizontalIcon />{t("shell:queue.sendNow")}</Button>}
    </>}
  </div>
}

function Row({ q, shown, ops }: { q: QRow; shown: string; ops: QOps }) {
  const e = useQueuedEdit(shown, ops), t = useT()
  return (
    <li className="flex flex-col gap-1 px-4 py-2 anim-in sm:flex-row sm:items-start sm:gap-3" data-queued={q.id}>
      <div className="min-w-0 flex-1">
        {e.edit !== null ? <EditBox e={e} />
          : q.via === "steve" ? <Brief text={shown} label={t("shell:queue.steveBrief")} className="type-callout text-muted-foreground" />
          : (shown !== "（附件）" || !q.files?.length) && <div className="line-clamp-4 break-words text-foreground" data-queued-text><Markdown text={shown} className="type-callout" /></div>}
        <Files files={q.files} className="mt-1.5" />
      </div>
      {!ops && <div className="flex shrink-0 items-center gap-1.5 py-1 type-footnote text-muted-foreground" data-queued-sending><Spinner className="size-3" />{t("shell:queue.submitting")}</div>}
      {ops && ops !== "none" && <Actions e={e} shown={shown} ops={ops} via={q.via} className="-ml-2 sm:ml-0" />}
    </li>
  )
}

/* ── an item's queued supplements live in the comments they came from ──
 * Every comment is also a queue row (“补充信息：” + its words) until the session takes it, so the item page shows the
 * queue state and controls on the comment and keeps no strip of its own. A row is paired with the latest unpaired
 * comment to the same session that has the same words and no reply yet; once paired the pair sticks, so editing the
 * queued words does not move the row. Rows that pair with nothing (Steve's, ones edited before a reload)
 * stay in the strip under the composer. */
const paired = new Map<string, string>()
export function queuedComments(tl: any[], sid: string | null, rows: RQueued[], prefix: string) {
  const byComment = new Map<number, RQueued>(), rest: RQueued[] = []
  if (!sid) return { byComment, rest: rows }
  const open = (n: number) => tl[n][0] === "me" && tl[n][3] === sid && !tl[n][4] && !byComment.has(n)
  const key = (n: number) => tl[n][2] + "\u0000" + tl[n][1]
  const find = (ok: (n: number) => boolean) => { let n = tl.length - 1; while (n >= 0 && !(open(n) && ok(n))) n--; return n }
  const strip = (t: string) => (t.startsWith(prefix) ? t.slice(prefix.length) : t)
  for (const q of [...rows].reverse()) {
    if (q.via === "steve") { rest.unshift(q); continue }
    const pin = paired.get(q.id)
    let n = pin === undefined ? -1 : find((n) => key(n) === pin)
    if (n < 0) n = find((n) => tl[n][1] === strip(q.text))
    if (n < 0) { rest.unshift(q); continue }
    paired.set(q.id, key(n)); byComment.set(n, q)
  }
  return { byComment, rest }
}

/** a comment still waiting in the session's queue: its words (editable), when it goes, and 编辑 / 撤回 / 立即发送 */
export function QueuedComment({ sid, q, prefix = "", composer, note, starting, at, body }: {
  sid: string; q: RQueued; prefix?: string; composer?: { current: ComposerHandle | null }; note: string; starting?: boolean; at: string; body: (text: string) => ReactNode
}) {
  const shown = prefix && q.text.startsWith(prefix) ? q.text.slice(prefix.length) : q.text
  const ops: Exclude<QOps, "none" | null> = {
    edit: (text) => qEdit(sid, q.id, (q.text.startsWith(prefix) ? prefix : "") + text),
    withdraw: () => qWithdraw(sid, q.id, composer?.current, (s) => (prefix && s.startsWith(prefix) ? s.slice(prefix.length) : s)),
    sendNow: starting ? undefined : () => qSendNow(sid, q.id),
  }
  const e = useQueuedEdit(shown, ops), t = useT()
  return <div className="min-w-0 flex-1" data-queued={q.id} data-queued-comment>
    {e.edit !== null ? <div className="mb-1"><EditBox e={e} /></div> : (shown !== "（附件）" || !q.files?.length) && body(shown)}
    <Files files={q.files} className="my-1" />
    <div className="flex flex-wrap items-center gap-x-2 type-footnote text-muted-foreground">
      <span className="inline-flex items-center gap-1" data-sq-note>{at} · <ClockIcon className="size-3 shrink-0" aria-hidden />{t("shell:queue.queuedNote", { note })}</span>
      <Actions e={e} shown={shown} ops={ops} via={q.via} className="-ml-2 sm:ml-0" />
    </div>
  </div>
}

/** while the Hub holds new turns for a release: what it waits for, and that new messages go after it */
export function HoldBanner() {
  const h = st.hold, t = useT()
  if (!h) return null
  const wait = h.running > 0 ? t("shell:queue.waitTurns", { count: h.running }) : t("shell:queue.restartNow")
  return (
    <div role="status" className="flex shrink-0 items-center gap-2 border-b bg-blue/8 px-4 py-2 type-footnote text-foreground/80 sm:px-6" data-hold>
      <RocketIcon className="size-3.5 shrink-0 text-blue" aria-hidden />
      <span className="min-w-0"><Trans t={t} i18nKey="shell:queue.hold" values={{ item: h.item, wait }} components={{ link: <a href={"#/items/" + h.item} className="font-mono tabular-nums underline-offset-2 hover:underline" /> }} /></span>
    </div>
  )
}
