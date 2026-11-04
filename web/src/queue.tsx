/* The queue strip under a composer: every message waiting for the session's next turn is its own row,
 * with 编辑 (inline), 撤回 (back into the composer when it is empty, else an undo toast) and 立即发送 (interrupts the
 * running turn; the queue, this message first, is the next turn). Attachments stay with the message.
 * `resumed:` rows were already delivered before restart: show recovery progress without queue mutations.
 * The main conversation has the same strip (ChatQueue): what you say while Steve is on a turn waits for it. */
import { useState } from "react"
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

function Row({ q, shown, ops }: { q: QRow; shown: string; ops: QOps }) {
  const resumed = q.via.startsWith("resumed:")
  const via = resumed ? q.via.slice("resumed:".length) : q.via
  const [edit, setEdit] = useState<string | null>(null), [busy, setBusy] = useState(false)
  const run = async (f: () => Promise<unknown>) => { setBusy(true); try { await f() } finally { setBusy(false) } }
  const save = () => run(async () => {
    if (resumed || !ops) return
    const t = (edit ?? "").trim()
    if (!t || t === shown) { setEdit(null); return }
    await ops.edit(t)
    setEdit(null) // saved, or already sent (the row is gone after the refresh)
  })
  return (
    <li className="flex flex-col gap-1 border-t border-dashed px-3.5 py-2 sm:flex-row sm:items-start sm:gap-3" data-queued={q.id} data-queued-resumed={resumed || undefined}>
      <div className="min-w-0 flex-1">
        {!resumed && edit !== null ? (
          <Textarea autoFocus value={edit} onChange={(e) => setEdit(e.target.value)} aria-label="修改排队的消息" className="min-h-14 bg-card text-[14px]" data-queued-input
            onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); setEdit(null) } if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); void save() } }} />
        ) : via === "steve" ? <Brief text={shown} label="Steve 交代" className="type-callout text-muted-foreground" />
          : (shown !== "（附件）" || !q.files?.length) && <div className="line-clamp-4 whitespace-pre-wrap break-words type-callout" data-queued-text>{shown}</div>}
        {resumed && <p className="mt-1 type-footnote text-muted-foreground" data-queued-recovery-note>重启前已送达，正在核对进度 / 等待续做</p>}
        <Files files={q.files} className="mt-1.5" />
      </div>
      {!resumed && !ops && <div className="flex shrink-0 items-center gap-1.5 py-1 type-footnote text-muted-foreground" data-queued-sending><Spinner className="size-3" />提交中</div>}
      {!resumed && ops && <div className="-ml-2 flex shrink-0 flex-wrap items-center gap-0.5 sm:ml-0">
        {edit !== null ? <>
          <Button size="xs" onClick={() => void save()} disabled={busy || !edit.trim()} data-queued-save>保存</Button>
          <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setEdit(null)} disabled={busy}>取消</Button>
        </> : <>
          {q.via !== "steve" && <Button size="xs" variant="ghost" className="text-muted-foreground" aria-label={"编辑排队的消息：" + snip(shown)} disabled={busy} onClick={() => setEdit(shown)} data-queued-edit><PencilIcon />编辑</Button>}
          <Button size="xs" variant="ghost" className="text-muted-foreground" aria-label={"撤回排队的消息：" + snip(shown)} disabled={busy} onClick={() => void run(ops.withdraw)} data-queued-withdraw><Undo2Icon />撤回</Button>
          {ops.sendNow && <Button size="xs" variant="ghost" className="text-blue" aria-label={"立即发送排队的消息：" + snip(shown)} disabled={busy} onClick={() => void run(ops.sendNow!)} data-queued-send><SendHorizontalIcon />立即发送</Button>}
        </>}
      </div>}
    </li>
  )
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
