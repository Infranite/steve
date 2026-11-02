/* IM conversations in 对话 (channels-personas.md §2.12): every IM space where Steve answers is a conversation of its
 * own. The switcher above the conversation lists them with their badge (provider · space · 群聊/私聊); one opens at
 * #/c/<space> with the same message parts as yours, the sender above each turn, and the “收到 → 完成” state of the
 * chat message (§2.11). Read-only here: a note links to the space, where a message goes out through the checks. */
import { useCallback, useEffect, useState } from "react"
import { cn } from "cn"
import { CheckIcon, LoaderIcon, MessageCircleIcon, UsersIcon, UserIcon, XIcon, ExternalLinkIcon } from "lucide-react"
import { conversationsApi, type ConvBadge, type ConvMessage, type ConvRow, type Reaction } from "@/api/conversations"
import { msgOf, useHubEvents, type Msg } from "@/store"
import { go } from "@/ui"
import { Button } from "@/components/ui/button"

/** the IM conversations (undefined: this Hub has none) */
export function useConversations() {
  const [rows, setRows] = useState<ConvRow[] | undefined>()
  const load = useCallback(() => { conversationsApi.list().then(setRows).catch(() => setRows(undefined)) }, [])
  useEffect(load, [load])
  useHubEvents(["message", "chan_message", "space"], load, 600)
  return rows
}

/** one IM conversation, its messages as the chat renders them plus what only IM turns have */
export function useConversation(space: string | undefined) {
  const [data, setData] = useState<{ badge: ConvBadge; msgs: Msg[]; raw: Map<string, ConvMessage> } | null | undefined>()
  const load = useCallback(() => {
    if (!space) return
    conversationsApi.messages(space).then((r) => {
      if (!r) { setData(null); return }
      setData({ badge: r.conversation, msgs: r.messages.map(msgOf), raw: new Map(r.messages.map((m) => [m.id, m])) })
    }).catch(() => setData(null))
  }, [space])
  useEffect(load, [load])
  useHubEvents(["message", "chan_message"], load, 250)
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

/** your conversation and the IM ones, as a row of chips above the conversation (scrolls sideways on a phone) */
export function ConvSwitcher({ current }: { current?: string }) {
  const rows = useConversations()
  if (!rows?.length) return null
  const chip = (on: boolean) => cn("flex h-8 shrink-0 items-center gap-1.5 rounded-full border px-3 type-footnote transition-colors", on ? "border-foreground/20 bg-accent text-foreground" : "text-muted-foreground hover:bg-accent/60")
  return (
    <nav aria-label="对话" className="mx-auto flex w-full max-w-[744px] shrink-0 gap-1.5 overflow-x-auto px-4 pb-2 [scrollbar-width:none] sm:px-8" data-conv-switcher>
      <button type="button" className={chip(!current)} aria-current={!current ? "page" : undefined} onClick={() => go("#/")} data-conv="steve">
        <MessageCircleIcon className="size-3.5" strokeWidth={1.75} />你和 Steve
      </button>
      {rows.map((r) => (
        <button key={r.space} type="button" className={chip(current === r.space)} aria-current={current === r.space ? "page" : undefined} onClick={() => go("#/c/" + r.space)} data-conv={r.space} title={`${r.provider} · ${r.name} · ${kindLabel(r)}`}>
          {r.dm ? <UserIcon className="size-3.5" strokeWidth={1.75} /> : <UsersIcon className="size-3.5" strokeWidth={1.75} />}
          <span className="max-w-32 truncate">{r.name}</span>
          <span className="text-muted-foreground/70">{r.provider}</span>
        </button>
      ))}
    </nav>
  )
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

/** what became of Steve's message in the space */
export function ChanOutNote({ m }: { m?: ConvMessage }) {
  const s = m?.meta?.chan_out?.status as string | undefined
  if (!s || s === "skipped") return null
  const t: Record<string, string> = { sent: "已发出", pending: "正在发出", draft: "等你批准后发出", blocked: "有机密值，已拦下", failed: "没有发出去", discarded: "你没有发出" }
  return <div className={cn("type-footnote", s === "failed" || s === "blocked" ? "text-destructive" : "text-muted-foreground")} data-chan-out={s}>{t[s] ?? s}</div>
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
