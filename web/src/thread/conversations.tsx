import { useDecisionView, useOpenDecisions } from "@/data/sync"
import { Textarea } from "@/components/ui/textarea"
import { Markdown } from "@/components/markdown"
/* IM conversations in 对话 (channels-personas.md §2.12): every IM space where Steve answers is a conversation of its
 * own. The switcher above the conversation lists them with their badge (provider · space · 群聊/私聊); one opens at
 * #/c/<space> with the same message parts as yours, the sender above each turn, and the “收到 → 完成” state of the
 * chat message (§2.11). Read-only here: a note links to the space, where a message goes out through the checks.
 * Since #243 a conversation also shows the chat's whole record (every message you may see, with what Steve did with
 * it), a space Steve never answered in is listed too, and a channel switcher picks the account whose conversations
 * the list shows. */
import { useCallback, useEffect, useRef, useState } from "react"
import { cn } from "cn"
import { ArrowLeftIcon, BotIcon, CheckIcon, LoaderIcon, UsersIcon, UserIcon, XIcon, ExternalLinkIcon, ListIcon, MessagesSquareIcon } from "lucide-react"
import { conversationsApi, type ChannelEntry, type ConvBadge, type ConvMessage, type ConvRecord, type ConvRow, type ConvState, type Handling, type Reaction, type RecordPage } from "@/api/conversations"
import { HANDLING_TONE, handlingDetail, shownUpTo } from "@/thread/channel-record"
import { st, msgOf, discardOut, decide, editReplyDraft, tm, useHubEvents, useShellSelector, type Msg } from "@/store"
import { SegmentedControl } from "@/components/affect/segmented-control-clean"
import { Badge } from "@/components/ui/badge"
import { ChannelMessageBody } from "@/channels/message-body"
import { matchesSpace } from "@/live-utils"
import { Empty, go, SearchField, Segmented } from "@/ui"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogTitle, DialogDescription } from "@/components/ui/dialog"
import { t as tr, useT } from "@/i18n"
import { HubError } from "@/api"
import { hubBase } from "@/hub"
import { MessageActions } from "@/components/affect/message-actions"
import { OpenInFeishu, openable, openInApp, openInFeishuLabel } from "@/channels/open-in-app"

/** records asked for at a time, and the most kept in view when the conversation reloads on a live update */
const RECORD_PAGE = 100, RECORD_KEEP = 500
/** fired once you were shown a conversation up to its newest message: the switcher's counts change */
export const READ_EV = "steve-conversation-read"

/** one IM conversation, its messages as the chat renders them plus what only IM turns have, and its full record (#243):
 *  every message of the chat you may see, newest page first, older pages on demand. Showing it moves your read marker. */
export function useConversation(space: string | undefined) {
  type Data = { badge: ConvBadge; msgs: Msg[]; raw: Map<string, ConvMessage>; records: ConvRecord[]; next: RecordPage["next"] }
  const workspace = st.workspace?.id, hub = hubBase()
  const scope = JSON.stringify([hub, workspace, space])
  const [state, setState] = useState<{ scope: string; data?: Data | null; error: boolean; loading: boolean; older: boolean }>({ scope, error: false, loading: !!space, older: false })
  const request = useRef(0), current = useRef(scope), kept = useRef(RECORD_PAGE), marked = useRef({ scope, at: 0 })
  current.current = scope
  /* what is on screen now, for a reload whose record failed */
  const shown = useRef<{ scope: string; data?: Data | null }>(state); shown.current = state
  const active = useCallback((id: number) => id === request.current && current.current === scope && st.workspace?.id === workspace && hubBase() === hub, [scope, workspace, hub])
  const markRead = useCallback((d: Data) => {
    if (!space) return
    const at = shownUpTo(d.msgs, d.records)
    if (marked.current.scope !== scope) marked.current = { scope, at: 0 }
    if (!at || at <= marked.current.at) return
    marked.current.at = at
    conversationsApi.read(space, at).then(() => window.dispatchEvent(new Event(READ_EV)), () => {})
  }, [space, scope])
  const load = useCallback(() => {
    const id = ++request.current
    if (!space) return
    setState(old => ({ scope, data: old.scope === scope ? old.data : undefined, error: false, loading: true, older: false }))
    if (current.current !== scope) kept.current = RECORD_PAGE
    /* the record only adds to the conversation: when it fails, what was shown of it stays and the turns still load */
    const record = conversationsApi.record(space, undefined, Math.min(RECORD_KEEP, kept.current)).then((r) => ({ ok: true as const, r }), () => ({ ok: false as const }))
    Promise.all([conversationsApi.messages(space), record]).then(([r, rec]) => {
      if (!active(id)) return
      const before = shown.current?.scope === scope ? shown.current.data : undefined
      const records = rec.ok ? rec.r?.records ?? [] : before?.records ?? []
      const next = rec.ok ? (rec.r?.has_more ? rec.r.next : null) : before?.next ?? null
      const data: Data = { badge: r.conversation, msgs: r.messages.map(msgOf), raw: new Map(r.messages.map((m) => [m.id, m])), records, next }
      setState({ scope, data, error: false, loading: false, older: false })
      markRead(data)
    }).catch((e) => {
      if (!active(id)) return
      // Other failures are retryable and retain the loaded messages.
      if (e instanceof HubError && e.status === 404) setState({ scope, data: null, error: false, loading: false, older: false })
      else setState(old => ({ scope, data: old.scope === scope ? old.data : undefined, error: true, loading: false, older: false }))
    })
  }, [space, scope, active, markRead])
  /** the page before the oldest record shown */
  const older = useCallback(() => {
    const d = state.scope === scope ? state.data : undefined
    if (!space || !d?.next || state.older) return
    const id = request.current
    setState(old => ({ ...old, older: true }))
    conversationsApi.record(space, d.next).then((rec) => {
      if (!active(id)) return
      setState(old => {
        if (old.scope !== scope || !old.data) return { ...old, older: false }
        const have = new Set(old.data.records.map((r) => r.id))
        const records = [...(rec?.records ?? []).filter((r) => !have.has(r.id)), ...old.data.records]
        kept.current = records.length
        return { ...old, older: false, data: { ...old.data, records, next: rec?.has_more ? rec.next : null } }
      })
    }).catch(() => { if (active(id)) setState(old => ({ ...old, older: false })) })
  }, [space, scope, state, active])
  useEffect(() => { load(); return () => { request.current++ } }, [load])
  /* a chat message's event names its space (`sp_…`), the conversation is `chan:sp_…` */
  useHubEvents(["message", "chan_message"], load, 250, { filter: (e) =>
    !!space && (e.type === "message" ? e.space === space : matchesSpace(e as { space_id?: string }, space.replace(/^chan:/, ""))) })
  // Hide the previous scope immediately, before the new effect starts its request.
  const mine = state.scope === scope
  return { data: mine ? state.data : undefined, error: mine && state.error, loading: mine ? state.loading : !!space, reload: load, older, loadingOlder: mine && state.older }
}

const kindLabel = (b: ConvBadge) => (b.dm ? tr("thread:conv.dm") : b.member_count ? tr("thread:conv.groupMembers", { count: b.member_count }) : tr("thread:conv.group"))

/** provider · space · group or DM (and 外部 when the space has outside members) */
export function ConvBadgeChip({ b, className }: { b: ConvBadge; className?: string }) {
  const I = b.dm ? UserIcon : UsersIcon
  const t = useT()
  return (
    <span className={cn("inline-flex min-w-0 items-center gap-1.5 type-footnote text-muted-foreground", className)} data-conv-badge>
      <span className="shrink-0 rounded-md bg-muted px-1.5 py-px type-label text-foreground/80">{b.provider}</span>
      <I className="size-3.5 shrink-0" strokeWidth={1.75} />
      <span className="truncate text-foreground/85">{b.name}</span>
      <span className="shrink-0">{kindLabel(b)}</span>
      {b.has_external && <span className="shrink-0 rounded-md bg-attention/12 px-1.5 py-px type-label text-attention">{t("thread:conv.external")}</span>}
    </span>
  )
}

const OPEN_EV = "steve-open-conversations"
/** the list of IM conversations, from the history panel (ST-50: the main conversation is not one of them) or the channel
 *  switcher, there with the account picked (`connection`) */
export function openConversations(connection?: string) { window.dispatchEvent(new CustomEvent(OPEN_EV, { detail: { connection: typeof connection === "string" ? connection : undefined } })) }

/** The channel accounts you see, for the switcher and the list's filter. */
export function useChannels() {
  const [list, setList] = useState<ChannelEntry[] | undefined>()
  const [error, setError] = useState("")
  const scope = st.workspace?.id, hub = hubBase()
  const request = useRef(0)
  const load = useCallback(() => {
    const id = ++request.current
    const active = () => id === request.current && st.workspace?.id === scope && hubBase() === hub
    conversationsApi.channels().then(
      (c) => { if (active()) { setList(c); setError("") } },
      (e: Error) => { if (active()) setError(e.message) },
    )
  }, [scope, hub])
  useEffect(() => { setList(undefined); setError(""); load(); return () => { request.current++ } }, [load])
  useEffect(() => { window.addEventListener(READ_EV, load); return () => window.removeEventListener(READ_EV, load) }, [load])
  useHubEvents(["chan_message", "space", "connection"], load, 1000, { maxWait: 5000 })
  return { channels: list, error, retry: load }
}

const channelOf = (t: EventTarget | null) => (t instanceof Element ? t.closest("[data-channel]")?.getAttribute("data-channel") ?? null : null)

/** 对话's channel switcher (#243): Steve, then every channel account you see with how many of its conversations have new
 *  messages (affectUI Segmented Control, scrolling sideways on a phone). Steve is your conversation; an account opens its
 *  conversations. Inside a channel conversation its account is the one picked. Nothing shows without a channel account. */
export function ChannelSwitcher({ channels, current }: { channels?: ChannelEntry[]; current?: ConvBadge | null }) {
  const t = useT()
  const pressed = useRef<string | null>(null)
  if (!channels?.length) return null
  /* undefined: your conversation; null: a channel conversation still loading */
  const value = current === undefined ? "steve" : current?.connection ?? ""
  const options = [
    { value: "steve", label: st.bots.steve?.n ?? "Steve", icon: <MessagesSquareIcon strokeWidth={1.75} />, data: { "data-channel": "steve" } },
    ...channels.map((c) => ({
      value: c.connection, label: c.name, count: c.unread || undefined, title: t(c.mode === "self" ? "thread:conv.channelSelf" : "thread:conv.channelBot", { provider: c.provider, name: c.name }),
      ariaLabel: c.unread ? t("thread:conv.channelUnread", { name: c.name, count: c.unread }) : c.name,
      icon: c.mode === "self" ? <UserIcon strokeWidth={1.75} /> : <BotIcon strokeWidth={1.75} />, data: { "data-channel": c.connection },
    })),
  ]
  return (
    /* the account already picked (the control ignores it) opens its conversations again; the control captures the
     * pointer on its picked option, so the option pressed is noted before that */
    <div className="mx-auto flex w-full max-w-[800px] shrink-0 px-4 pt-2 sm:px-8" data-channel-switcher
      onPointerDownCapture={(e) => { pressed.current = channelOf(e.target) }}
      onClick={(e) => { const c = pressed.current ?? channelOf(e.target); pressed.current = null; if (c && c === value && c !== "steve") openConversations(c) }}>
      <SegmentedControl options={options} value={value} onChange={(v) => (v === "steve" ? go("#/") : openConversations(v))} size="md" variant="tray" aria-label={t("thread:conv.channels")} className="steve-seg max-w-full" />
    </div>
  )
}

/** A bounded navigator; closing only changes this user's list, never the channel. Inside an IM conversation a bar
 *  leads back to the main conversation; otherwise only the list (opened from 历史) */
export function ConvSwitcher({ channels, current, badge }: { channels?: ChannelEntry[]; current?: string; badge?: ConvBadge }) {
  const t = useT()
  const [open, setOpen] = useState(false)
  /* the account whose conversations are listed; "" all of them */
  const [account, setAccount] = useState("")
  useEffect(() => {
    const h = (e: Event) => { const c = (e as CustomEvent<{ connection?: string } | null>).detail?.connection; if (typeof c === "string") setAccount(c); setOpen(true) }
    window.addEventListener(OPEN_EV, h); return () => window.removeEventListener(OPEN_EV, h)
  }, [])
  const [query, setQuery] = useState("")
  const [state, setState] = useState<ConvState>("open")
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
    conversationsApi.page(query, state, offset, account).then(r => {
      if (id !== request.current || !alive.current || st.workspace?.id !== scope) return
      setRows(old => offset ? [...old, ...r.conversations.filter(x => !old.some(y => y.space === x.space))] : r.conversations)
      setNext(r.has_more ? r.next_offset ?? offset + r.conversations.length : null)
    }).catch(e => { if (id === request.current && alive.current && st.workspace?.id === scope) setError(e.message || tr("thread:conv.loadFailed")) })
      .finally(() => { if (id === request.current && alive.current && st.workspace?.id === scope) setLoading(false) })
  }, [query, state, scope, account])
  latest.current = { load, open }
  useEffect(() => {
    request.current++; setRows([]); setNext(null)
    if (!open) return
    setLoading(true)
    const timer = setTimeout(() => load(), 180)
    return () => { clearTimeout(timer); request.current++ }
  }, [open, load])
  const refresh = useCallback(() => { if (open) setUpdates(true) }, [open])
  useHubEvents(["message", "chan_message", "space", "patch"], refresh, 600)
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
        <span className="truncate type-callout font-medium" title={badge?.name}>{badge?.name || t("thread:conv.current")}</span>
        <Button variant="ghost" size="icon-sm" className="shrink-0 text-muted-foreground" aria-label={t("thread:conv.closeCurrent")} disabled={busy !== null} onClick={() => void change(current, true)}><XIcon /></Button>
      </div>
      <OpenInFeishu url={badge?.open_url} compact />
      <Button variant="ghost" size="sm" className="ml-auto shrink-0 text-muted-foreground" onClick={() => setOpen(true)}><ListIcon />{t("thread:conv.list")}</Button>
    </nav>}
    {!open && error && <p role="alert" className="mx-auto px-4 type-callout text-destructive">{error}</p>}
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent className="max-h-[85dvh] gap-4 sm:max-w-xl" data-conversation-manager>
        <div className="flex flex-col gap-1"><DialogTitle>{t("thread:conv.list")}</DialogTitle>
        <DialogDescription>{t("thread:conv.listHelp")}</DialogDescription></div>
        {!!channels?.length && <Segmented pressed className="max-w-full" label={t("thread:conv.channels")} value={account} onChange={setAccount}
          options={[["", t("thread:conv.allChannels")] as const, ...channels.map((c) => [c.connection, c.name, t(c.mode === "self" ? "thread:conv.channelSelf" : "thread:conv.channelBot", { provider: c.provider, name: c.name })] as const)]} />}
        <SearchField aria-label={t("thread:conv.search")} placeholder={t("thread:conv.searchPlaceholder")} value={query} onChange={e => setQuery(e.target.value)} />
        <Segmented pressed className="max-w-full" label={t("thread:conv.stateLabel")} value={state} onChange={setState} options={(["open", "closed", "all", "replied"] as const).map((key) => [key, t(`thread:conv.state.${key}`)] as const)} />
        {updates && <Button size="sm" variant="secondary" className="w-fit justify-self-start anim-in" onClick={() => load()}>{t("thread:conv.updated")}</Button>}
        {error && <div role="alert" className="flex items-center gap-2 type-callout text-destructive">{error}<Button size="sm" variant="outline" onClick={() => load()}>{t("common:action.retry")}</Button></div>}
        <div className="-mx-2 min-h-24 overflow-y-auto overscroll-contain px-2" style={{ maxHeight: "48dvh" }} aria-busy={loading}>
          {rows.map(r => <div key={r.space} className="flex min-w-0 items-center gap-2 border-b border-border/70 py-1 last:border-b-0 anim-in" data-conversation-row={r.space}>
            <button type="button" className="-mx-1 flex min-w-0 flex-1 flex-col gap-0.5 rounded-lg px-2.5 py-2 text-left hover:bg-accent disabled:opacity-50" disabled={busy !== null} onClick={async () => {
              const hash = location.hash
              if (r.closed) {
                setBusy(r.space); setError("")
                try { await conversationsApi.preference(r.space, false) } catch (e) { if (active()) setError(e instanceof Error ? e.message : tr("thread:conv.restoreFailed")); return } finally { if (active()) setBusy(null) }
              }
              if (!active() || !latest.current.open || location.hash !== hash) return
              go("#/c/" + r.space); setOpen(false)
            }} aria-label={r.closed ? t("thread:conv.restoreOpen", { name: r.name }) : t("thread:conv.openNamed", { name: r.name })}>
              <div className="flex min-w-0 items-baseline gap-2"><span className="truncate type-body font-medium">{r.name}</span><span className="min-w-0 shrink truncate type-footnote text-tertiary">{t(r.closed ? "thread:conv.rowMetaClosed" : "thread:conv.rowMeta", { provider: r.provider, kind: kindLabel(r) })}</span>
                {r.last > 0 && <time className="ml-auto shrink-0 type-footnote tabular-nums text-tertiary" dateTime={new Date(r.last).toISOString()} data-conversation-last>{tm(r.last)}</time>}</div>
              <div className="flex min-w-0 items-center gap-2"><span className="min-w-0 flex-1 truncate type-callout text-muted-foreground">{r.last_text || t("thread:conv.noPreview")}</span>
                {!!r.unread && <Badge tone="info" count={r.unread} aria-label={t("thread:conv.unread", { count: r.unread })} data-conversation-unread={r.unread} />}</div>
            </button>
            <Button size="sm" variant="ghost" className="shrink-0 text-muted-foreground" disabled={busy !== null} onClick={() => void change(r.space, !r.closed)} aria-label={r.closed ? t("thread:conv.restoreNamed", { name: r.name }) : t("thread:conv.closeNamed", { name: r.name })}>{busy === r.space ? t("thread:conv.saving") : r.closed ? t("thread:conv.restore") : t("common:action.close")}</Button>
          </div>)}
          {!rows.length && !loading && !error && <Empty icon={<MessagesSquareIcon />} title={query || state === "replied" ? t("thread:conv.noMatch") : state === "closed" ? t("thread:conv.noClosed") : t("thread:conv.none")} />}
          {loading && <p role="status" className="py-3 text-center type-callout text-tertiary">{t("thread:conv.loading")}</p>}
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
  if (r?.error) return <span className="type-footnote break-words text-muted-foreground" data-reaction="unconfirmed" title={r.error}>{t("thread:reaction.unconfirmed", { error: r.error })}</span>
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
const CHAN_OUT = { sent: "thread:chanOut.sent", pending: "thread:chanOut.pending", draft: "thread:chanOut.draft", failed: "thread:chanOut.failed", blocked: "thread:chanOut.blocked", unknown: "thread:chanOut.unknown", discarded: "thread:chanOut.discarded", disconnected: "thread:chanOut.disconnected", skipped: "thread:chanOut.notSent" } as const

/** what became of Steve's message in the space. A draft is closed right here, in its own conversation; sending it is
 *  approved on its card in 待处理, which shows the text that would actually go out and any disclosure warning */
export function ChanOutNote({ m }: { m?: ConvMessage }) {
  const t = useT()
  const [closed, setClosed] = useState(false)
  const s = m?.meta?.chan_out?.status as string | undefined, decision = m?.meta?.chan_out?.decision as string | undefined
  const tier = useDecisionView(decision)?.tier
  const busy = useShellSelector(() => !!decision && st.pendingActions.has("decision:" + decision))
  const reason = m?.meta?.chan_out?.reason as string | undefined
  /* a new state from the Hub replaces what this click assumed */
  useEffect(() => setClosed(false), [s, decision])
  if (!s || (s === "skipped" && !reason)) return null
  /* only a draft can be assumed closed: whatever the Hub says past it wins over a late click */
  const shown = closed && s === "draft" ? "discarded" : s
  const label = shown in CHAN_OUT ? t(CHAN_OUT[shown as keyof typeof CHAN_OUT]) : shown
  const note = <div className={cn("type-callout", shown === "failed" || shown === "disconnected" ? "text-destructive" : "text-muted-foreground")} data-chan-out={shown}>{reason ? t("thread:chanOut.withReason", { label, reason }) : label}</div>
  if (shown !== "draft" || !decision) return note
  if (tier === "draft") return null
  const discard = async () => { if (await discardOut(decision)) setClosed(true) }
  return (
    <div className="flex flex-wrap items-center gap-2" data-chan-out-decision={decision}>
      {note}
      <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void discard()} data-chan-out-discard>{t("thread:chanOut.discard")}</Button>
      <Button size="xs" variant="outline" disabled={busy} onClick={() => go("#/inbox")} data-chan-out-review>{t("thread:chanOut.review")}</Button>
    </div>
  )
}

/** The decision tier owns the single draft shown on your side of this conversation. */
export function ConversationDraft({ conv, decisionIds }: { conv: string; decisionIds: ReadonlySet<string> }) {
  const decisions = useOpenDecisions()
  const draft = decisions.findLast((d) => d.tier === "draft" &&
    (d.space_id === conv.replace(/^chan:/, "") || decisionIds.has(d.id)))
  return draft ? <DraftBubble key={draft.id} id={draft.id} text={draft.draft} /> : null
}

function DraftBubble({ id, text }: { id: string; text: string }) {
  const t = useT(), [edit, setEdit] = useState<{ previous: string; text: string } | null>(null)
  const [busy, setBusy] = useState(false)
  const run = async (action: () => Promise<unknown>) => {
    setBusy(true)
    try { if (await action()) setEdit(null) } finally { setBusy(false) }
  }
  return <section className="w-fit max-w-[85%] self-end rounded-xl border border-dashed border-blue/50 bg-blue/5 p-3" data-conversation-draft={id}>
    <div className="mb-2 type-footnote font-semibold text-blue">{t("thread:chanOut.draftBubble")}</div>
    {edit ? <Textarea autoFocus aria-label={t("thread:chanOut.draftBubble")} value={edit.text} maxLength={4000}
      onChange={(e) => setEdit({ ...edit, text: e.target.value })} /> : <Markdown text={text} />}
    <div className="mt-2 flex flex-wrap justify-end gap-2">
      {edit ? <>
        <Button size="sm" variant="ghost" disabled={busy} onClick={() => setEdit(null)}>{t("common:action.cancel")}</Button>
        <Button size="sm" disabled={busy || !edit.text.trim()} onClick={() => void run(() => editReplyDraft(id, edit.previous, edit.text))}>{t("common:action.save")}</Button>
      </> : <>
        <Button size="sm" variant="ghost" disabled={busy} onClick={() => void run(() => discardOut(id))} data-draft-skip>{t("thread:chanOut.discard")}</Button>
        <Button size="sm" variant="outline" disabled={busy} onClick={() => setEdit({ previous: text, text })} data-draft-edit>{t("common:action.edit")}</Button>
        <Button size="sm" disabled={busy || !text.trim()} onClick={() => void run(() => decide("", "yes", text, id))} data-draft-send>{t("thread:chanOut.send")}</Button>
      </>}
    </div>
  </section>
}

/** whose turn it is: the sender's name over the bubble */
export function SenderLine({ m }: { m?: ConvMessage }) {
  const t = useT()
  const c = m?.meta?.chan
  if (!c) return null
  return <div className="flex items-center justify-end gap-2 self-end type-footnote text-tertiary"><span className="font-medium text-muted-foreground">{c.sender || t("thread:who.someone")}{c.command ? " · /bot" : ""}</span><ReactionChip r={m?.reaction} /></div>
}

/** in place of the composer: this conversation is read here, said there */
export function ImReadOnly({ b }: { b: ConvBadge }) {
  const t = useT()
  return (
    <div className="flex flex-col gap-2 rounded-xl bg-muted/60 px-4 py-3 sm:flex-row sm:items-center" data-im-readonly>
      <div className="min-w-0 flex-1 type-callout text-muted-foreground">
        {t(b.dm ? "thread:conv.readOnlyDm" : "thread:conv.readOnlyGroup", { provider: b.provider, name: b.name })}
      </div>
      <Button size="sm" variant="outline" className="shrink-0 self-start sm:self-auto" onClick={() => go("#/channels/space/" + b.space_id)} data-open-space>
        <ExternalLinkIcon />{t(b.dm ? "thread:conv.openDm" : "thread:conv.openGroup")}
      </Button>
    </div>
  )
}

/** the actions under a turn of an IM conversation (affectUI Message Actions): open it in its platform's client. `side`:
 *  where the turn sits (someone in the chat on the right, Steve on the left); on a touch screen only the latest shows */
export function ImMsgActions({ url, side, last }: { url?: string | null; side: "start" | "end"; last?: boolean }) {
  const t = useT()
  const href = openable(url)
  if (!href) return null
  return (
    <div className={cn("pointer-events-none absolute -bottom-8 left-0 right-0 opacity-0 transition-opacity [&_button]:pointer-events-auto focus-within:opacity-100",
      side === "end" ? "group-hover/me:opacity-100 group-focus-within/me:opacity-100" : "group-hover/sm:opacity-100 group-focus-within/sm:opacity-100",
      last && "[@media(hover:none)]:opacity-100")} data-im-msg-actions>
      <MessageActions actions={[]} align={side} quiet={false} label={t("pages:chat.msgActions")} more={[{ key: "open-in-app", label: openInFeishuLabel(), icon: <ExternalLinkIcon strokeWidth={1.75} />, onClick: () => openInApp(href), attrs: { "data-open-in-app": "" } }]} />
    </div>
  )
}

/* a handling's name; read when it renders so it follows the language */
const HANDLING: Record<Exclude<Handling["state"], "none">, `thread:record.${string}`> = {
  answered: "thread:record.answered", drafted: "thread:record.drafted", blocked: "thread:record.blocked", unknown: "thread:record.unknown", recalled: "thread:record.recalled", local: "thread:record.local", waiting: "thread:record.waiting", item: "thread:record.item", working: "thread:record.working",
  failed: "thread:record.failed", skipped: "thread:record.skipped", decided: "thread:record.decided", recorded: "thread:record.recorded",
}

/** what Steve did with a received message (#243): its state as a badge, the reason after it, and the way to its item or
 *  to the card waiting for you, or to your own conversation when it was answered there. Nothing when nothing is known. `side`: where the message sits; `reacted`: a turn whose
 *  reaction chip already says it is being answered or was */
export function HandlingLine({ h, side = "end", reacted = false }: { h?: Handling; side?: "start" | "end"; reacted?: boolean }) {
  const t = useT()
  if (!h || h.state === "none" || (reacted && h.state === "working")) return null
  const tone = HANDLING_TONE[h.state] ?? "neutral", detail = handlingDetail(h)
  return (
    <div className={cn("flex min-w-0 max-w-full flex-wrap items-center gap-x-2 gap-y-1 type-footnote", side === "end" ? "justify-end self-end text-right" : "self-start")} data-handling={h.state}>
      <Badge tone={tone} dot live={h.state === "working"}>{t(HANDLING[h.state] as "thread:record.answered")}</Badge>
      {h.state === "failed" && h.out_status === "sent" && <span className="text-muted-foreground">{t("thread:record.failureNoticeSent")}</span>}
      {detail && <span className={cn("min-w-0 break-words", h.state === "failed" ? "text-destructive" : "text-muted-foreground")} data-handling-detail>{detail}</span>}
      {h.item && <Button size="xs" variant="ghost" className="font-mono tabular-nums" onClick={() => go("#/items/" + h.item)} data-handling-item={h.item}>{h.item}</Button>}
      {h.decision && <Button size="xs" variant="outline" onClick={() => go("#/inbox")} data-handling-decision={h.decision}>{t("thread:record.open")}</Button>}
      {h.main_turn && <Button size="xs" variant="ghost" onClick={() => go("#/")} data-handling-main={h.main_turn}>{t("thread:record.inMain", { name: st.bots.steve?.n ?? "Steve" })}</Button>}
    </div>
  )
}

/* a sent message's status → its label; the rest show as they are */
const OUT_LABEL = { ...CHAN_OUT, recalled: "thread:chanOut.recalled" } as const

/** one message of the chat that has no turn in the conversation (#243): who said it and when, what it said, and what
 *  became of it — received ones on the right like the turns, sent ones on the left like Steve's */
export function RecordRow({ r, last }: { r: ConvRecord; last?: boolean }) {
  const t = useT()
  const mine = r.direction === "out", side = mine ? "start" : "end"
  const out = r.out, outKey = out && out.status in OUT_LABEL ? OUT_LABEL[out.status as keyof typeof OUT_LABEL] : undefined
  const failed = out?.status === "failed" || out?.status === "disconnected"
  return (
    <div className={cn("relative flex min-w-0 flex-col gap-1.5", mine ? "group/sm items-start" : "group/me items-end")} data-msg={r.id} data-record={r.direction}>
      <div className={cn("flex min-w-0 max-w-full items-baseline gap-2 px-1 type-footnote text-tertiary", mine ? "self-start" : "self-end")}>
        <span className="truncate font-medium text-muted-foreground">{r.sender.name || t("thread:who.someone")}</span>
        <time className="shrink-0 tabular-nums" dateTime={new Date(r.created).toISOString()}>{tm(r.created)}</time>
        {r.history && <span className="shrink-0">{t("thread:record.history")}</span>}
      </div>
      <div className={cn("w-full min-w-0 max-w-[600px] rounded-2xl px-3.5 py-2.5", mine ? "self-start bg-card shadow-[var(--lift)]" : "self-end bg-muted/70")}><ChannelMessageBody message={r} /></div>
      {r.handling && <HandlingLine h={r.handling} side={side} />}
      {out && out.status !== "sent" && <div className={cn("flex min-w-0 flex-wrap items-center gap-2 type-footnote", failed ? "text-destructive" : "text-muted-foreground")} data-chan-out={out.status}>
        <span>{outKey ? t(outKey) : out.status}</span>
        {out.error && <span className="min-w-0 break-words">{out.error}</span>}
        {out.decision && <Button size="xs" variant="outline" onClick={() => go("#/inbox")} data-record-decision={out.decision}>{t("thread:record.open")}</Button>}
      </div>}
      <ImMsgActions url={r.open_url} side={side} last={last} />
    </div>
  )
}

/** the top of a channel conversation: earlier messages of the chat, a page at a time */
export function OlderRecords({ more, loading, onMore }: { more: boolean; loading: boolean; onMore: () => void }) {
  const t = useT()
  if (!more) return null
  return (
    <div className="-mb-4 flex min-h-5 justify-center type-footnote text-tertiary" data-record-older={loading ? "loading" : "more"}>
      {loading ? <span className="flex items-center gap-2"><LoaderIcon className="size-3.5 animate-spin" />{t("pages:chat.loadingOlder")}</span>
        : <button type="button" className="hover:text-foreground" onClick={onMore}>{t("thread:record.older")}</button>}
    </div>
  )
}
