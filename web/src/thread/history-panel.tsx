/* ST-50 历史: finding something said earlier without leaving the conversation. A search over the whole conversation (and
 * item numbers), or its topics by day; a row jumps to the message, the conversation offers the way back. Beside the
 * conversation on wide screens (in place of the rail), a sheet otherwise. IM conversations are one row away. */
import { useEffect, useMemo, useRef, useState } from "react"
import { cn } from "cn"
import { HistoryIcon, ListIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet"
import { Spinner } from "@/components/ui/spinner"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { setHistory, useRail, useWide, wide } from "@/chat2"
import { st, useStore, type Msg } from "@/store"
import { openConversations } from "@/thread/conversations"
import { historyState, jumpToMsg, loadOlder, mainThread, searchThread, useHistory } from "@/thread/history"
import { snippet, topicsOf, type Topic } from "@/thread/topics"
import { lang, t as tr, useT } from "@/i18n"
import { date, hm } from "@/i18n/format"
import { SearchField, SidePanel, PanelHead, Empty } from "@/ui"

const DAY = 86400_000
export function dayLabel(ts: number) {
  const d = new Date(ts), today = new Date(); today.setHours(0, 0, 0, 0)
  const diff = today.getTime() - new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime()
  if (diff <= 0) return tr("thread:history.today")
  if (diff <= DAY) return tr("thread:history.yesterday")
  return date(d, { ...(d.getFullYear() !== today.getFullYear() && { year: "numeric" }), month: "short", day: "numeric" })
}
function byDay<T>(xs: T[], at: (x: T) => number): [string, T[]][] {
  const out: [string, T[]][] = []
  for (const x of xs) { const l = dayLabel(at(x)); const last = out[out.length - 1]; if (last?.[0] === l) last[1].push(x); else out.push([l, [x]]) }
  return out
}

export function HistoryToggle() {
  const { historyOpen } = useRail()
  const t = useT()
  return (
    <Tooltip>
      <TooltipTrigger render={<Button variant="ghost" size="sm" aria-label={t("thread:history.title")} aria-pressed={historyOpen} onClick={() => setHistory(!historyOpen)} className="gap-1.5 px-2 text-muted-foreground aria-pressed:bg-muted aria-pressed:text-foreground" data-history-toggle />}>
        <HistoryIcon /><span className="max-sm:sr-only">{t("thread:history.title")}</span>
      </TooltipTrigger>
      <TooltipContent>{t("thread:history.tooltip")}</TooltipContent>
    </Tooltip>
  )
}

/** beside the conversation on wide screens, a sheet on narrow ones */
export function HistoryPanel() {
  const { historyOpen } = useRail()
  const isWide = useWide()
  const t = useT()
  if (isWide) return <SidePanel open={historyOpen} test={{ "data-history": "aside" }}><HistoryBody /></SidePanel>
  return (
    <Sheet open={historyOpen} onOpenChange={setHistory}>
      <SheetContent side="right" className="w-[88vw] max-w-[380px]! gap-0 p-0" showCloseButton={false} data-history="sheet"><SheetTitle className="sr-only">{t("thread:history.title")}</SheetTitle><HistoryBody /></SheetContent>
    </Sheet>
  )
}

function HistoryBody() {
  useStore(); useHistory()
  const t = useT()
  const [q, setQ] = useState(""), [hits, setHits] = useState<Msg[] | null>(null), [local, setLocal] = useState(false), [busy, setBusy] = useState(false), [at, setAt] = useState<string | null>(null)
  const input = useRef<HTMLInputElement>(null)
  useEffect(() => { if (wide()) input.current?.focus() }, [])
  useEffect(() => {
    const x = q.trim()
    if (!x) { setHits(null); setBusy(false); return }
    const ac = new AbortController(); setBusy(true)
    const timer = setTimeout(() => searchThread(x, ac.signal).then((r) => { setHits(r.msgs); setLocal(r.local) }).catch(() => {}).finally(() => { if (!ac.signal.aborted) setBusy(false) }), 220)
    return () => { clearTimeout(timer); ac.abort() }
  }, [q])
  const list = mainThread(st.thread)
  const topics = useMemo(() => topicsOf(list), [list, lang()])
  const { more, ended, loading } = historyState()
  const open = (id: string, created?: number) => { setAt(id); jumpToMsg(id, created); if (!wide()) setHistory(false) }
  return (
    <div className="flex min-h-0 flex-1 flex-col" data-history-body>
      <PanelHead title={t("thread:history.title")} closeLabel={t("thread:history.close")} onClose={() => setHistory(false)} closeTest="data-history-close" data-history-head />
      <SearchField className="mx-4 mb-1 mt-4 shrink-0" inputRef={input} value={q} onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => { if (e.key === "Escape" && q) { e.stopPropagation(); setQ("") } }} placeholder={t("thread:history.searchPlaceholder")} aria-label={t("thread:history.searchLabel")} data-history-search
        trailing={busy ? <Spinner className="size-3.5 text-muted-foreground" /> : hits && <span className="shrink-0 type-footnote tabular-nums text-tertiary" data-history-count>{t("thread:history.hits", { count: hits.length })}</span>} />
      <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain pb-3" data-history-list>
        {hits ? <>
          {local && <div className="px-4 pt-2 type-footnote text-tertiary">{t("thread:history.localOnly")}</div>}
          {!hits.length && !busy && <Empty compact className="mx-4 mt-3" title={t("thread:history.notFound", { q: q.trim() })} data-history-empty />}
          {byDay(hits, (m) => m.created ?? 0).map(([day, ms]) => <section key={day}><div className="px-4 pb-1.5 pt-4 type-label text-tertiary">{day}</div>
            {ms.map((m) => <Hit key={m.id} m={m} q={q.trim()} on={at === m.id} onClick={() => open(m.id!, m.created)} />)}</section>)}
        </> : <>
          {byDay(topics, (x) => x.at).map(([day, ts]) => <section key={day}><div className="px-4 pb-1.5 pt-4 type-label text-tertiary">{day}</div>
            {ts.map((x) => <TopicRow key={x.id} t={x} on={at === x.id} onClick={() => open(x.id, x.at)} />)}</section>)}
          {!topics.length && <Empty compact className="mx-4 mt-3" title={t("thread:history.empty")} />}
        </>}
      </div>
      <div className="flex shrink-0 items-center gap-1 border-t border-sidebar-border px-2 py-1.5">
        {!hits && (more ? <Button variant="ghost" size="sm" className="text-muted-foreground" disabled={loading} onClick={() => void loadOlder()} data-history-older>{loading && <Spinner />}{t("thread:history.older")}</Button>
          : ended && <span className="px-2 type-footnote text-tertiary" data-history-start>{t("thread:history.start")}</span>)}
        <Button variant="ghost" size="sm" className="ml-auto text-muted-foreground" onClick={() => { if (!wide()) setHistory(false); openConversations() }} data-open-conversations><ListIcon />{t("thread:conv.list")}</Button>
      </div>
    </div>
  )
}

function Row({ on, onClick, children, test }: { on: boolean; onClick: () => void; children: React.ReactNode; test: Record<string, string> }) {
  return <button type="button" onClick={onClick} aria-current={on || undefined} className={cn("mx-2 flex w-[calc(100%-16px)] min-w-0 flex-col gap-0.5 rounded-lg px-2 py-2 text-left transition-colors hover:bg-sidebar-accent", on && "bg-sidebar-accent")} {...test}>{children}</button>
}

function Hit({ m, q, on, onClick }: { m: Msg; q: string; on: boolean; onClick: () => void }) {
  const s = snippet(m.t ?? "", q), t = useT()
  const text = s.at < 0 ? s.text : <>{s.text.slice(0, s.at)}<mark className="rounded-md bg-blue/15 px-0.5 text-foreground">{s.text.slice(s.at, s.at + s.len)}</mark>{s.text.slice(s.at + s.len)}</>
  return (
    <Row on={on} onClick={onClick} test={{ "data-history-hit": m.id! }}>
      <span className="flex min-w-0 items-baseline gap-2 type-footnote text-tertiary"><span className="font-medium text-muted-foreground">{m.k === "me" ? t("thread:who.me") : st.bots.steve?.n ?? "Steve"}</span>{m.item && <span className="font-mono">{m.item}</span>}<span className="ml-auto shrink-0 tabular-nums">{m.created ? hm(m.created) : ""}</span></span>
      <span className="line-clamp-2 wrap-anywhere type-callout text-foreground/85">{text}</span>
    </Row>
  )
}

function TopicRow({ t: topic, on, onClick }: { t: Topic; on: boolean; onClick: () => void }) {
  const t = useT()
  return (
    <Row on={on} onClick={onClick} test={{ "data-history-topic": topic.id }}>
      <span className="flex min-w-0 items-baseline gap-2"><span className="min-w-0 flex-1 truncate type-callout text-foreground">{topic.title}</span><span className="shrink-0 type-footnote tabular-nums text-tertiary">{hm(topic.at)}</span></span>
      <span className="truncate type-footnote text-tertiary">{topic.items.length ? t("thread:topic.countItems", { count: topic.count, items: topic.items.slice(0, 3).join(" ") }) : t("thread:topic.count", { count: topic.count })}</span>
    </Row>
  )
}
