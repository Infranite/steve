/* IM conversations in 对话 (channels-personas.md §2.12): every IM space where Steve answers is a conversation of its
 * own. The switcher above the conversation lists them with their badge (provider · space · 群聊/私聊); one opens at
 * #/c/<space> with the same message parts as yours, the sender above each turn, and the “收到 → 完成” state of the
 * chat message (§2.11). Read-only here: a note links to the space, where a message goes out through the checks. */
import { useCallback, useEffect, useRef, useState } from "react"
import { cn } from "cn"
import { CheckIcon, LoaderIcon, MessageCircleIcon, UsersIcon, UserIcon, XIcon, ExternalLinkIcon, ListIcon } from "lucide-react"
import { conversationsApi, type ConvBadge, type ConvMessage, type ConvRow, type Reaction } from "@/api/conversations"
import { st, msgOf, discardOut, useHubEvents, type Msg } from "@/store"
import { matchesSpace } from "@/live-utils"
import { go } from "@/ui"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Dialog, DialogContent, DialogTitle, DialogDescription } from "@/components/ui/dialog"

/** one IM conversation, its messages as the chat renders them plus what only IM turns have */
export function useConversation(space: string | undefined) {
  const [data, setData] = useState<{ badge: ConvBadge; msgs: Msg[]; raw: Map<string, ConvMessage> } | null | undefined>()
  const request = useRef(0)
  const load = useCallback(() => {
    const id = ++request.current
    if (!space) return
    conversationsApi.messages(space).then((r) => {
      if (id !== request.current) return
      if (!r) { setData(null); return }
      setData({ badge: r.conversation, msgs: r.messages.map(msgOf), raw: new Map(r.messages.map((m) => [m.id, m])) })
    }).catch(() => { if (id === request.current) setData(null) })
  }, [space])
  useEffect(() => { setData(undefined); load(); return () => { request.current++ } }, [load])
  /* a chat message's event names its space (`sp_…`), the conversation is `chan:sp_…` */
  useHubEvents(["message", "chan_message"], load, 250, { filter: (e) => matchesSpace(e as { space_id?: string }, space?.replace(/^chan:/, "")) })
  return data
}

const kindLabel = (b: ConvBadge) => (b.dm ? "私聊" : `群聊${b.member_count ? ` · ${b.member_count} 人` : ""}`)

/** provider · space · group or DM (and 外部 when the space has outside members) */
export function ConvBadgeChip({ b, className }: { b: ConvBadge; className?: string }) {
  const I = b.dm ? UserIcon : UsersIcon
  return (
    <span className={cn("inline-flex min-w-0 items-center gap-1.5 type-footnote text-muted-foreground", className)} data-conv-badge>
      <span className="shrink-0 rounded-md bg-accent px-1.5 py-px font-medium text-foreground">{b.provider}</span>
      <I className="size-3.5 shrink-0" strokeWidth={1.75} />
      <span className="truncate text-foreground">{b.name}</span>
      <span className="shrink-0">{kindLabel(b)}</span>
      {b.has_external && <span className="shrink-0 rounded-md border px-1 py-px">外部</span>}
    </span>
  )
}

/** A bounded navigator; closing only changes this user's list, never the channel. */
export function ConvSwitcher({ current, badge }: { current?: string; badge?: ConvBadge }) {
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState("")
  const [state, setState] = useState<"open" | "closed" | "all">("open")
  const [rows, setRows] = useState<ConvRow[]>([])
  const [next, setNext] = useState<number | null>(null)
  const [loading, setLoading] = useState(false)
  const [updates, setUpdates] = useState(false)
  const [error, setError] = useState("")
  const [busy, setBusy] = useState<string | null>(null)
  const request = useRef(0)
  const alive = useRef(false)
  const scope = st.workspace?.id
  const active = () => alive.current && st.workspace?.id === scope
  const latest = useRef({ load: (_offset = 0) => {}, open })
  useEffect(() => { alive.current = true; return () => { alive.current = false; request.current++ } }, [])
  const load = useCallback((offset = 0) => {
    const id = ++request.current
    setLoading(true); setError(""); setUpdates(false)
    conversationsApi.page(query, state, offset).then(r => {
      if (id !== request.current || !alive.current || st.workspace?.id !== scope) return
      setRows(old => offset ? [...old, ...r.conversations.filter(x => !old.some(y => y.space === x.space))] : r.conversations)
      setNext(r.has_more ? r.next_offset ?? offset + r.conversations.length : null)
    }).catch(e => { if (id === request.current && alive.current && st.workspace?.id === scope) setError(e.message || "对话载入失败") })
      .finally(() => { if (id === request.current && alive.current && st.workspace?.id === scope) setLoading(false) })
  }, [query, state, scope])
  latest.current = { load, open }
  useEffect(() => {
    request.current++; setRows([]); setNext(null)
    if (!open) return
    setLoading(true)
    const timer = setTimeout(() => load(), 180)
    return () => { clearTimeout(timer); request.current++ }
  }, [open, load])
  const refresh = useCallback(() => { if (open) setUpdates(true) }, [open])
  useHubEvents(["message", "chan_message", "space"], refresh, 600)
  const change = async (space: string, closed: boolean) => {
    const hash = location.hash
    setBusy(space); setError("")
    try {
      await conversationsApi.preference(space, closed)
      if (!active()) return
      if (closed && current === space && location.hash === hash) go("#/")
      if (latest.current.open) latest.current.load()
    } catch (e) { if (active()) setError(e instanceof Error ? e.message : "保存失败，请重试") }
    finally { if (active()) setBusy(null) }
  }
  return <>
    <nav aria-label="对话" className="mx-auto flex w-full max-w-[800px] shrink-0 items-center gap-2 px-4 pb-2 pt-3 sm:px-8" data-conv-switcher>
      <Button variant={!current ? "secondary" : "ghost"} size="sm" onClick={() => go("#/")} aria-current={!current ? "page" : undefined} data-conv="steve"><MessageCircleIcon />你和 Steve</Button>
      {current && <div className="flex min-w-0 flex-1 items-center gap-1 rounded-lg border pl-2" data-current-conversation>
        <span className="truncate type-footnote" title={badge?.name}>{badge?.name || "当前对话"}</span>
        <Button variant="ghost" size="icon-sm" className="ml-auto shrink-0" aria-label="关闭当前对话" disabled={busy !== null} onClick={() => void change(current, true)}><XIcon /></Button>
      </div>}
      <Button variant="outline" size="sm" className="ml-auto shrink-0" onClick={() => setOpen(true)}><ListIcon />会话列表</Button>
    </nav>
    {!open && error && <p role="alert" className="mx-auto px-4 text-destructive type-footnote">{error}</p>}
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent className="max-h-[85dvh] gap-3 sm:max-w-xl" data-conversation-manager>
        <DialogTitle>会话列表</DialogTitle>
        <DialogDescription>关闭只收起你列表中的会话，消息和渠道继续保留。可在“已关闭”中恢复。</DialogDescription>
        <Input aria-label="搜索会话" placeholder="搜索名称、渠道或最近消息" value={query} onChange={e => setQuery(e.target.value)} />
        <div className="flex gap-2" aria-label="会话状态">{([['open','未关闭'],['closed','已关闭'],['all','全部']] as const).map(([key,label]) => <Button key={key} size="sm" variant={state === key ? "secondary" : "ghost"} aria-pressed={state === key} onClick={() => setState(key)}>{label}</Button>)}</div>
        {updates && <Button size="sm" variant="outline" onClick={() => load()}>会话有更新，刷新列表</Button>}
        {error && <div role="alert" className="flex items-center gap-2 text-destructive">{error}<Button size="sm" variant="outline" onClick={() => load()}>重试</Button></div>}
        <div className="min-h-24 overflow-y-auto overscroll-contain" style={{ maxHeight: "48dvh" }} aria-busy={loading}>
          {rows.map(r => <div key={r.space} className="flex min-w-0 items-center gap-2 border-b py-2" data-conversation-row={r.space}>
            <button type="button" className="min-w-0 flex-1 rounded-md p-2 text-left hover:bg-accent disabled:opacity-50" disabled={busy !== null} onClick={async () => {
              const hash = location.hash
              if (r.closed) {
                setBusy(r.space); setError("")
                try { await conversationsApi.preference(r.space, false) } catch (e) { if (active()) setError(e instanceof Error ? e.message : "恢复失败"); return } finally { if (active()) setBusy(null) }
              }
              if (!active() || !latest.current.open || location.hash !== hash) return
              go("#/c/" + r.space); setOpen(false)
            }} aria-label={(r.closed ? "恢复并打开：" : "打开：") + r.name}>
              <div className="truncate font-medium">{r.name}</div>
              <div className="truncate type-footnote text-muted-foreground">{r.provider} · {kindLabel(r)}{r.closed ? " · 已关闭" : ""}</div>
              <div className="truncate type-footnote text-muted-foreground">{r.last_text || "暂无消息摘要"}</div>
            </button>
            <Button size="sm" variant="ghost" disabled={busy !== null} onClick={() => void change(r.space, !r.closed)} aria-label={(r.closed ? "恢复：" : "关闭：") + r.name}>{busy === r.space ? "保存中…" : r.closed ? "恢复" : "关闭"}</Button>
          </div>)}
          {!rows.length && !loading && !error && <p className="py-8 text-center text-muted-foreground">{query ? "没有匹配的会话" : state === "closed" ? "没有已关闭的会话" : "暂无会话"}</p>}
          {loading && <p role="status" className="py-3 text-center text-muted-foreground">正在载入…</p>}
          {next !== null && <Button className="mt-3 w-full" variant="outline" disabled={loading} onClick={() => load(next)}>加载更多</Button>}
        </div>
      </DialogContent>
    </Dialog>
  </>
}

const REACT: Record<Reaction["state"], [string, typeof CheckIcon | null, string] | null> = {
  ack: ["处理中", LoaderIcon, "text-muted-foreground"],
  done: ["已回答", CheckIcon, "text-success"],
  failed: ["没答上", XIcon, "text-destructive"],
  cleared: null,
}

/** the chat message's reaction, as shown in the chat (§2.11) */
export function ReactionChip({ r }: { r?: Reaction }) {
  const x = r ? REACT[r.state] : null
  if (!r || !x) return null
  const [label, I, tone] = x
  const by = r.as_user ? "以你本人" : "机器人"
  return (
    <span className={cn("inline-flex items-center gap-1 type-footnote", tone)} title={`${by}在消息上${r.state === "ack" ? "加了“处理中”" : `换成了“${label}”`}${r.error ? `（飞书没有收下：${r.error}）` : ""}`} data-reaction={r.state}>
      {I && <I className={cn("size-3.5", r.state === "ack" && "animate-spin [animation-duration:2s]")} strokeWidth={2} />}{label}
    </span>
  )
}

/** what became of Steve's message in the space. A draft is closed right here, in its own conversation; sending it is
 *  approved on its card in 待处理, which shows the text that would actually go out and any disclosure warning */
export function ChanOutNote({ m }: { m?: ConvMessage }) {
  const [closed, setClosed] = useState(false)
  const s = m?.meta?.chan_out?.status as string | undefined, decision = m?.meta?.chan_out?.decision as string | undefined
  /* a new state from the Hub replaces what this click assumed */
  useEffect(() => setClosed(false), [s, decision])
  if (!s || s === "skipped") return null
  const t: Record<string, string> = { sent: "已发出", pending: "正在发出", draft: "等你批准后发出", blocked: "有机密值，已拦下", failed: "没有发出去", unknown: "可能已经发出，正在核对", discarded: "你没有发出" }
  /* only a draft can be assumed closed: whatever the Hub says past it wins over a late click */
  const shown = closed && s === "draft" ? "discarded" : s
  const note = <div className={cn("type-footnote", shown === "failed" || shown === "blocked" ? "text-destructive" : "text-muted-foreground")} data-chan-out={shown}>{t[shown] ?? shown}</div>
  if (shown !== "draft" || !decision) return note
  const busy = st.pendingActions.has("decision:" + decision)
  const discard = async () => { if (await discardOut(decision)) setClosed(true) }
  return (
    <div className="flex flex-wrap items-center gap-2" data-chan-out-decision={decision}>
      {note}
      <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void discard()} data-chan-out-discard>不发出</Button>
      <Button size="xs" variant="outline" disabled={busy} onClick={() => go("#/inbox")} data-chan-out-review>去批准…</Button>
    </div>
  )
}

/** whose turn it is: the sender's name over the bubble */
export function SenderLine({ m }: { m?: ConvMessage }) {
  const c = m?.meta?.chan
  if (!c) return null
  return <div className="flex items-center justify-end gap-2 self-end type-footnote text-muted-foreground"><span>{c.sender || "某人"}{c.command ? " · /bot" : ""}</span><ReactionChip r={m?.reaction} /></div>
}

/** in place of the composer: this conversation is read here, said there */
export function ImReadOnly({ b }: { b: ConvBadge }) {
  return (
    <div className="flex flex-col gap-2 rounded-2xl border px-4 py-3 sm:flex-row sm:items-center" data-im-readonly>
      <div className="min-w-0 flex-1 type-callout text-muted-foreground">
        这是{b.provider}「{b.name}」里的对话，在这里只读。要在{b.dm ? "私聊" : "群"}里说话，去{b.dm ? "私聊" : "群"}页手动发送，同样经过披露检查。
      </div>
      <Button size="sm" variant="outline" className="shrink-0 self-start sm:self-auto" onClick={() => go("#/channels/space/" + b.space_id)} data-open-space>
        <ExternalLinkIcon />打开{b.dm ? "私聊" : "群"}页
      </Button>
    </div>
  )
}
