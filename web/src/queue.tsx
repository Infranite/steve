/* The queue strip under a composer: every message waiting for the session's next turn is its own row,
 * with 编辑 (inline), 撤回 (back into the composer when it is empty, else an undo toast) and 立即发送 (interrupts the
 * running turn; the queue, this message first, is the next turn). Attachments stay with the message.
 * `resumed:` rows were already delivered before restart: show recovery progress without queue mutations.
 * The main conversation has the same strip (ChatQueue): what you say while Steve is on a turn waits for it.
 * On an item page the queue is shown on the comments instead (QueuedComment); the strip keeps only what pairs with none. */
import { useState, type ReactNode } from "react"
import { ClockIcon, PencilIcon, RocketIcon, SendHorizontalIcon, Undo2Icon } from "lucide-react"
import { Spinner } from "@/components/ui/spinner"
import { Button } from "@/components/ui/button"
import { Textarea } from "@/components/ui/textarea"
import { Files } from "@/attach"
import type { RQueued } from "@/api"
import { chatBusy, qEdit, qSendNow, qWithdraw, st, threadQueuedEdit, threadQueuedWithdraw, type Sess } from "@/store"
import { Brief, type ComposerHandle } from "@/ui"

/** when the queue goes, in the strip's header */
export function queueNote(s: Sess | null) {
  if (st.hold) return "等发布完成后发送"
  if (s?.st === "run") return "本回合结束后发送"
  return "会话没在运行，可以立即发送"
}

const snip = (t: string) => (t.length > 24 ? t.slice(0, 24) + "…" : t)

/** `prefix`: what the Hub put in front of the user's words (item supplements: “补充信息：”); shown and edited without it */
export function QueueStrip({ sid, rows, note, composer, prefix = "", className }: {
  sid: string; rows: RQueued[]; note: string; composer?: { current: ComposerHandle | null }; prefix?: string; className?: string
}) {
  if (!rows.length) return null
  const resumedCount = rows.filter(q => q.via.startsWith("resumed:")).length
  const queuedCount = rows.length - resumedCount
  const strip = (t: string) => (prefix && t.startsWith(prefix) ? t.slice(prefix.length) : t)
  return (
    <div role="region" aria-label={resumedCount ? "待发送与恢复中的消息" : "排队的消息"} className={"mb-2 rounded-xl border border-dashed " + (className ?? "")} data-sq>
      <div className="flex items-center gap-2 px-3.5 pt-2 pb-1 type-footnote text-muted-foreground" data-sq-note>
        <ClockIcon className="size-3.5 shrink-0" aria-hidden /><span>{resumedCount > 0 && <>待核对 {resumedCount} 条{queuedCount > 0 ? "；" : ""}</>}{queuedCount > 0 && <>排队 {queuedCount} 条 · {note}</>}</span>
      </div>
      <ul className="flex flex-col">
        {rows.map((q) => <Row key={q.id} q={q} shown={strip(q.text)} ops={{
          edit: (t) => qEdit(sid, q.id, (q.text.startsWith(prefix) ? prefix : "") + t),
          withdraw: () => qWithdraw(sid, q.id, composer?.current, strip),
          sendNow: () => qSendNow(sid, q.id),
        }} />)}
      </ul>
    </div>
  )
}

/** the main conversation's queue: messages said while Steve was on a turn, sent one by one once it ends */
export function ChatQueue({ composer }: { composer?: { current: ComposerHandle | null } }) {
  const rows = st.queue
  if (!rows.length) return null
  const note = st.hold ? "等发布完成后发送" : chatBusy() && (st.thinking || st.live) ? "本回合结束后依次发送" : "马上发送"
  return (
    <div role="region" aria-label="排队的消息" className="mb-2 rounded-xl border border-dashed" data-sq data-chat-queue>
      <div className="flex items-center gap-2 px-3.5 pt-2 pb-1 type-footnote text-muted-foreground" data-sq-note>
        <ClockIcon className="size-3.5 shrink-0" aria-hidden /><span>排队 {rows.length} 条 · {note}</span>
      </div>
      <ul className="flex flex-col">
        {rows.map((m) => <Row key={m.id ?? m.clientId} q={{ id: m.id ?? "", text: m.t ?? "", files: m.files, via: "me" }} shown={m.t ?? ""} ops={m.id ? {
          edit: (t) => threadQueuedEdit(m.id!, t),
          withdraw: () => threadQueuedWithdraw(m.id!, composer?.current),
        } : null} />)}
      </ul>
    </div>
  )
}

type QRow = Pick<RQueued, "id" | "text" | "files" | "via">
/** what a row can do; `null` while it is still being sent */
type QOps = { edit: (text: string) => Promise<unknown>; withdraw: () => Promise<unknown>; sendNow?: () => Promise<unknown> } | null

/** inline editing of one queued message: `edit` is the draft while editing, else null */
function useQueuedEdit(shown: string, ops: QOps, locked = false) {
  const [edit, setEdit] = useState<string | null>(null), [busy, setBusy] = useState(false)
  const run = async (f: () => Promise<unknown>) => { setBusy(true); try { await f() } finally { setBusy(false) } }
  const save = () => run(async () => {
    if (locked || !ops) return
    const t = (edit ?? "").trim()
    if (!t || t === shown) { setEdit(null); return }
    await ops.edit(t)
    setEdit(null) // saved, or already sent (the row is gone after the refresh)
  })
  return { edit, setEdit, busy, run, save }
}
type QEdit = ReturnType<typeof useQueuedEdit>

function EditBox({ e }: { e: QEdit }) {
  return <Textarea autoFocus value={e.edit ?? ""} onChange={(x) => e.setEdit(x.target.value)} aria-label="修改排队的消息" className="min-h-14 bg-card text-[14px]" data-queued-input
    onKeyDown={(x) => { if (x.key === "Escape") { x.preventDefault(); e.setEdit(null) } if (x.key === "Enter" && (x.metaKey || x.ctrlKey)) { x.preventDefault(); void e.save() } }} />
}

function Actions({ e, shown, ops, via, className }: { e: QEdit; shown: string; ops: NonNullable<QOps>; via: string; className?: string }) {
  const { edit, busy, run } = e
  return <div className={"flex shrink-0 flex-wrap items-center gap-0.5 " + (className ?? "")}>
    {edit !== null ? <>
      <Button size="xs" onClick={() => void e.save()} disabled={busy || !edit.trim()} data-queued-save>保存</Button>
      <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => e.setEdit(null)} disabled={busy}>取消</Button>
    </> : <>
      {via !== "steve" && <Button size="xs" variant="ghost" className="text-muted-foreground" aria-label={"编辑排队的消息：" + snip(shown)} disabled={busy} onClick={() => e.setEdit(shown)} data-queued-edit><PencilIcon />编辑</Button>}
      <Button size="xs" variant="ghost" className="text-muted-foreground" aria-label={"撤回排队的消息：" + snip(shown)} disabled={busy} onClick={() => void run(ops.withdraw)} data-queued-withdraw><Undo2Icon />撤回</Button>
      {ops.sendNow && <Button size="xs" variant="ghost" className="text-blue" aria-label={"立即发送排队的消息：" + snip(shown)} disabled={busy} onClick={() => void run(ops.sendNow!)} data-queued-send><SendHorizontalIcon />立即发送</Button>}
    </>}
  </div>
}

function Row({ q, shown, ops }: { q: QRow; shown: string; ops: QOps }) {
  const resumed = q.via.startsWith("resumed:")
  const via = resumed ? q.via.slice("resumed:".length) : q.via
  const e = useQueuedEdit(shown, ops, resumed)
  return (
    <li className="flex flex-col gap-1 border-t border-dashed px-3.5 py-2 sm:flex-row sm:items-start sm:gap-3" data-queued={q.id} data-queued-resumed={resumed || undefined}>
      <div className="min-w-0 flex-1">
        {!resumed && e.edit !== null ? <EditBox e={e} />
          : via === "steve" ? <Brief text={shown} label="Steve 交代" className="type-callout text-muted-foreground" />
          : (shown !== "（附件）" || !q.files?.length) && <div className="line-clamp-4 whitespace-pre-wrap break-words type-callout" data-queued-text>{shown}</div>}
        {resumed && <p className="mt-1 type-footnote text-muted-foreground" data-queued-recovery-note>重启前已送达，正在核对进度 / 等待续做</p>}
        <Files files={q.files} className="mt-1.5" />
      </div>
      {!resumed && !ops && <div className="flex shrink-0 items-center gap-1.5 py-1 type-footnote text-muted-foreground" data-queued-sending><Spinner className="size-3" />提交中</div>}
      {!resumed && ops && <Actions e={e} shown={shown} ops={ops} via={q.via} className="-ml-2 sm:ml-0" />}
    </li>
  )
}

/* ── an item's queued supplements live in the comments they came from ──
 * Every comment is also a queue row (“补充信息：” + its words) until the session takes it, so the item page shows the
 * queue state and controls on the comment and keeps no strip of its own. A row is paired with the latest unpaired
 * comment to the same session that has the same words and no reply yet; once paired the pair sticks, so editing the
 * queued words does not move the row. Rows that pair with nothing (Steve's, recovering ones, edited before a reload)
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
    if (q.via === "steve" || q.via.startsWith("resumed:")) { rest.unshift(q); continue }
    const pin = paired.get(q.id)
    let n = pin === undefined ? -1 : find((n) => key(n) === pin)
    if (n < 0) n = find((n) => tl[n][1] === strip(q.text))
    if (n < 0) { rest.unshift(q); continue }
    paired.set(q.id, key(n)); byComment.set(n, q)
  }
  return { byComment, rest }
}

/** a comment still waiting in the session's queue: its words (editable), when it goes, and 编辑 / 撤回 / 立即发送 */
export function QueuedComment({ sid, q, prefix = "", composer, note, at, body }: {
  sid: string; q: RQueued; prefix?: string; composer?: { current: ComposerHandle | null }; note: string; at: string; body: (text: string) => ReactNode
}) {
  const shown = prefix && q.text.startsWith(prefix) ? q.text.slice(prefix.length) : q.text
  const ops: NonNullable<QOps> = {
    edit: (t) => qEdit(sid, q.id, (q.text.startsWith(prefix) ? prefix : "") + t),
    withdraw: () => qWithdraw(sid, q.id, composer?.current, (t) => (prefix && t.startsWith(prefix) ? t.slice(prefix.length) : t)),
    sendNow: () => qSendNow(sid, q.id),
  }
  const e = useQueuedEdit(shown, ops)
  return <div className="min-w-0 flex-1" data-queued={q.id} data-queued-comment>
    {e.edit !== null ? <div className="mb-1"><EditBox e={e} /></div> : (shown !== "（附件）" || !q.files?.length) && body(shown)}
    <Files files={q.files} className="my-1" />
    <div className="flex flex-wrap items-center gap-x-2 type-footnote text-muted-foreground">
      <span className="inline-flex items-center gap-1" data-sq-note>{at} · <ClockIcon className="size-3 shrink-0" aria-hidden />排队中，{note}</span>
      <Actions e={e} shown={shown} ops={ops} via={q.via} className="-ml-2 sm:ml-0" />
    </div>
  </div>
}

/** while the Hub holds new turns for a release: what it waits for, and that new messages go after it */
export function HoldBanner() {
  const h = st.hold
  if (!h) return null
  const wait = h.running > 0 ? `等 ${h.running} 个回合结束` : "马上重启"
  return (
    <div role="status" className="flex shrink-0 items-center gap-2 border-b bg-blue/8 px-4 py-2 type-footnote text-foreground/80 sm:px-6" data-hold>
      <RocketIcon className="size-3.5 shrink-0 text-blue" aria-hidden />
      <span className="min-w-0">正在发布 <a href={"#/items/" + h.item} className="font-mono tabular-nums underline-offset-2 hover:underline">{h.item}</a>，{wait}；新消息发布后发送</span>
    </div>
  )
}
