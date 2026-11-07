/* IM conversations in 对话 (channels-personas.md §2.12): every IM space where Steve answers is a conversation of its
 * own. The switcher above the conversation lists them with their badge (provider · space · 群聊/私聊); one opens at
 * #/c/<space> with the same message parts as yours, the sender above each turn, and the “收到 → 完成” state of the
 * chat message (§2.11). Read-only here: a note links to the space, where a message goes out through the checks. */
import { useCallback, useEffect, useRef, useState } from "react"
import { cn } from "cn"
import { ArrowLeftIcon, CheckIcon, LoaderIcon, UsersIcon, UserIcon, XIcon, ExternalLinkIcon, ListIcon } from "lucide-react"
import { conversationsApi, type ConvBadge, type ConvMessage, type ConvRow, type Reaction } from "@/api/conversations"
import { st, msgOf, discardOut, useHubEvents, type Msg } from "@/store"
import { matchesSpace } from "@/live-utils"
import { go } from "@/ui"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Dialog, DialogContent, DialogTitle, DialogDescription } from "@/components/ui/dialog"
import { t as tr, useT } from "@/i18n"

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

const kindLabel = (b: ConvBadge) => (b.dm ? tr("thread:conv.dm") : b.member_count ? tr("thread:conv.groupMembers", { count: b.member_count }) : tr("thread:conv.group"))

/** provider · space · group or DM (and 外部 when the space has outside members) */
export function ConvBadgeChip({ b, className }: { b: ConvBadge; className?: string }) {
  const I = b.dm ? UserIcon : UsersIcon
  const t = useT()
  return (
    <span className={cn("inline-flex min-w-0 items-center gap-1.5 type-footnote text-muted-foreground", className)} data-conv-badge>
      <span className="shrink-0 rounded-md bg-accent px-1.5 py-px font-medium text-foreground">{b.provider}</span>
      <I className="size-3.5 shrink-0" strokeWidth={1.75} />
      <span className="truncate text-foreground">{b.name}</span>
      <span className="shrink-0">{kindLabel(b)}</span>
      {b.has_external && <span className="shrink-0 rounded-md border px-1 py-px">{t("thread:conv.external")}</span>}
    </span>
  )
}

const OPEN_EV = "steve-open-conversations"
/** the list of IM conversations, from the history panel (ST-50: the main conversation is not one of them) */
export function openConversations() { window.dispatchEvent(new Event(OPEN_EV)) }

/** A bounded navigator; closing only changes this user's list, never the channel. Inside an IM conversation a bar
 *  leads back to the main conversation; otherwise only the list (opened from 历史) */
export function ConvSwitcher({ current, badge }: { current?: string; badge?: ConvBadge }) {
  const t = useT()
  const [open, setOpen] = useState(false)
  useEffect(() => { const h = () => setOpen(true); window.addEventListener(OPEN_EV, h); return () => window.removeEventListener(OPEN_EV, h) }, [])
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
    }).catch(e => { if (id === request.current && alive.current && st.workspace?.id === scope) setError(e.message || tr("thread:conv.loadFailed")) })
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
    } catch (e) { if (active()) setError(e instanceof Error ? e.message : tr("thread:conv.saveFailed")) }
    finally { if (active()) setBusy(null) }
  }
  return <>
    {current && <nav aria-label={t("thread:conv.nav")} className="mx-auto flex w-full max-w-[800px] shrink-0 items-center gap-2 px-4 pb-1 pt-2 sm:px-8" data-conv-switcher>
      <Button variant="ghost" size="sm" className="-ml-2 text-muted-foreground" onClick={() => go("#/")} data-conv="steve"><ArrowLeftIcon />{t("thread:conv.main")}</Button>
      <div className="flex min-w-0 flex-1 items-center gap-1" data-current-conversation>
        <span className="truncate type-footnote" title={badge?.name}>{badge?.name || t("thread:conv.current")}</span>
        <Button variant="ghost" size="icon-sm" className="shrink-0 text-muted-foreground" aria-label={t("thread:conv.closeCurrent")} disabled={busy !== null} onClick={() => void change(current, true)}><XIcon /></Button>
      </div>
      <Button variant="ghost" size="sm" className="ml-auto shrink-0 text-muted-foreground" onClick={() => setOpen(true)}><ListIcon />{t("thread:conv.list")}</Button>
    </nav>}
    {!open && error && <p role="alert" className="mx-auto px-4 text-destructive type-footnote">{error}</p>}
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent className="max-h-[85dvh] gap-3 sm:max-w-xl" data-conversation-manager>
        <DialogTitle>{t("thread:conv.list")}</DialogTitle>
        <DialogDescription>{t("thread:conv.listHelp")}</DialogDescription>
        <Input aria-label={t("thread:conv.search")} placeholder={t("thread:conv.searchPlaceholder")} value={query} onChange={e => setQuery(e.target.value)} />
        <div className="flex gap-2" aria-label={t("thread:conv.stateLabel")}>{(["open", "closed", "all"] as const).map((key) => <Button key={key} size="sm" variant={state === key ? "secondary" : "ghost"} aria-pressed={state === key} onClick={() => setState(key)}>{t(`thread:conv.state.${key}`)}</Button>)}</div>
        {updates && <Button size="sm" variant="outline" onClick={() => load()}>{t("thread:conv.updated")}</Button>}
        {error && <div role="alert" className="flex items-center gap-2 text-destructive">{error}<Button size="sm" variant="outline" onClick={() => load()}>{t("common:action.retry")}</Button></div>}
        <div className="min-h-24 overflow-y-auto overscroll-contain" style={{ maxHeight: "48dvh" }} aria-busy={loading}>
          {rows.map(r => <div key={r.space} className="flex min-w-0 items-center gap-2 border-b py-2" data-conversation-row={r.space}>
            <button type="button" className="min-w-0 flex-1 rounded-md p-2 text-left hover:bg-accent disabled:opacity-50" disabled={busy !== null} onClick={async () => {
              const hash = location.hash
              if (r.closed) {
                setBusy(r.space); setError("")
                try { await conversationsApi.preference(r.space, false) } catch (e) { if (active()) setError(e instanceof Error ? e.message : tr("thread:conv.restoreFailed")); return } finally { if (active()) setBusy(null) }
              }
              if (!active() || !latest.current.open || location.hash !== hash) return
              go("#/c/" + r.space); setOpen(false)
            }} aria-label={r.closed ? t("thread:conv.restoreOpen", { name: r.name }) : t("thread:conv.openNamed", { name: r.name })}>
              <div className="truncate font-medium">{r.name}</div>
              <div className="truncate type-footnote text-muted-foreground">{t(r.closed ? "thread:conv.rowMetaClosed" : "thread:conv.rowMeta", { provider: r.provider, kind: kindLabel(r) })}</div>
              <div className="truncate type-footnote text-muted-foreground">{r.last_text || t("thread:conv.noPreview")}</div>
            </button>
            <Button size="sm" variant="ghost" disabled={busy !== null} onClick={() => void change(r.space, !r.closed)} aria-label={r.closed ? t("thread:conv.restoreNamed", { name: r.name }) : t("thread:conv.closeNamed", { name: r.name })}>{busy === r.space ? t("thread:conv.saving") : r.closed ? t("thread:conv.restore") : t("common:action.close")}</Button>
          </div>)}
          {!rows.length && !loading && !error && <p className="py-8 text-center text-muted-foreground">{query ? t("thread:conv.noMatch") : state === "closed" ? t("thread:conv.noClosed") : t("thread:conv.none")}</p>}
          {loading && <p role="status" className="py-3 text-center text-muted-foreground">{t("thread:conv.loading")}</p>}
          {next !== null && <Button className="mt-3 w-full" variant="outline" disabled={loading} onClick={() => load(next)}>{t("thread:conv.more")}</Button>}
        </div>
      </DialogContent>
    </Dialog>
  </>
}

/* the label is a key, read when the chip renders so it follows the language */
const REACT: Record<Reaction["state"], ["thread:reaction.ack" | "thread:reaction.done" | "thread:reaction.failed", typeof CheckIcon | null, string] | null> = {
  ack: ["thread:reaction.ack", LoaderIcon, "text-muted-foreground"],
  done: ["thread:reaction.done", CheckIcon, "text-success"],
  failed: ["thread:reaction.failed", XIcon, "text-destructive"],
  cleared: null,
}

/** the chat message's reaction, as shown in the chat (§2.11) */
export function ReactionChip({ r }: { r?: Reaction }) {
  const t = useT()
  const x = r ? REACT[r.state] : null
  if (!r || !x) return null
  const [key, I, tone] = x, label = t(key)
  const said = r.state === "ack" ? t(r.as_user ? "thread:reaction.ackAsUser" : "thread:reaction.ackAsBot") : t(r.as_user ? "thread:reaction.changedAsUser" : "thread:reaction.changedAsBot", { label })
  return (
    <span className={cn("inline-flex items-center gap-1 type-footnote", tone)} title={r.error ? t("thread:reaction.withError", { text: said, error: r.error }) : said} data-reaction={r.state}>
      {I && <I className={cn("size-3.5", r.state === "ack" && "animate-spin [animation-duration:2s]")} strokeWidth={2} />}{label}
    </span>
  )
}

/* a chan_out status from the Hub to its label; an unknown status shows as it is */
const CHAN_OUT = { sent: "thread:chanOut.sent", pending: "thread:chanOut.pending", draft: "thread:chanOut.draft", blocked: "thread:chanOut.blocked", failed: "thread:chanOut.failed", unknown: "thread:chanOut.unknown", discarded: "thread:chanOut.discarded", disconnected: "thread:chanOut.disconnected" } as const

/** what became of Steve's message in the space. A draft is closed right here, in its own conversation; sending it is
 *  approved on its card in 待处理, which shows the text that would actually go out and any disclosure warning */
export function ChanOutNote({ m }: { m?: ConvMessage }) {
  const t = useT()
  const [closed, setClosed] = useState(false)
  const s = m?.meta?.chan_out?.status as string | undefined, decision = m?.meta?.chan_out?.decision as string | undefined
  /* a new state from the Hub replaces what this click assumed */
  useEffect(() => setClosed(false), [s, decision])
  if (!s || s === "skipped") return null
  /* only a draft can be assumed closed: whatever the Hub says past it wins over a late click */
  const shown = closed && s === "draft" ? "discarded" : s
  const note = <div className={cn("type-footnote", shown === "failed" || shown === "blocked" || shown === "disconnected" ? "text-destructive" : "text-muted-foreground")} data-chan-out={shown}>{shown in CHAN_OUT ? t(CHAN_OUT[shown as keyof typeof CHAN_OUT]) : shown}</div>
  if (shown !== "draft" || !decision) return note
  const busy = st.pendingActions.has("decision:" + decision)
  const discard = async () => { if (await discardOut(decision)) setClosed(true) }
  return (
    <div className="flex flex-wrap items-center gap-2" data-chan-out-decision={decision}>
      {note}
      <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void discard()} data-chan-out-discard>{t("thread:chanOut.discard")}</Button>
      <Button size="xs" variant="outline" disabled={busy} onClick={() => go("#/inbox")} data-chan-out-review>{t("thread:chanOut.review")}</Button>
    </div>
  )
}

/** whose turn it is: the sender's name over the bubble */
export function SenderLine({ m }: { m?: ConvMessage }) {
  const t = useT()
  const c = m?.meta?.chan
  if (!c) return null
  return <div className="flex items-center justify-end gap-2 self-end type-footnote text-muted-foreground"><span>{c.sender || t("thread:who.someone")}{c.command ? " · /bot" : ""}</span><ReactionChip r={m?.reaction} /></div>
}

/** in place of the composer: this conversation is read here, said there */
export function ImReadOnly({ b }: { b: ConvBadge }) {
  const t = useT()
  return (
    <div className="flex flex-col gap-2 rounded-2xl border px-4 py-3 sm:flex-row sm:items-center" data-im-readonly>
      <div className="min-w-0 flex-1 type-callout text-muted-foreground">
        {t(b.dm ? "thread:conv.readOnlyDm" : "thread:conv.readOnlyGroup", { provider: b.provider, name: b.name })}
      </div>
      <Button size="sm" variant="outline" className="shrink-0 self-start sm:self-auto" onClick={() => go("#/channels/space/" + b.space_id)} data-open-space>
        <ExternalLinkIcon />{t(b.dm ? "thread:conv.openDm" : "thread:conv.openGroup")}
      </Button>
    </div>
  )
}
