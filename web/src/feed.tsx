import { useEffect, useRef, useState } from "react"
import { ChevronDownIcon, ChevronUpIcon, ExternalLinkIcon, RefreshCwIcon } from "lucide-react"
import { feedApi, appendFeed, feedHref, type FeedClient, type FeedEntry, type FeedFilter, type FeedKind, type FeedPage, type FeedRead, type FeedSource } from "@/api/feed"
import { errText } from "@/api/flow"
import { HubError } from "@/api"
import { lang, useT } from "@/i18n"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/affect/checkbox-clean"
import { Select } from "@/components/affect/select-clean"
import { EmptyState } from "@/components/affect/empty-state-clean"
import { Markdown } from "@/components/markdown"
import { Loading, Page, Segmented } from "@/ui"

export function FeedView({ workspace, filter }: { workspace: string; filter: string }) {
  const [client] = useState(() => feedApi.forWorkspace(workspace))
  const [unread, setUnread] = useState(false)
  const [kind, setKind] = useState<FeedKind | "">(filter === "completion" || filter === "report" ? filter : "")
  const [source, setSource] = useState<FeedSource | "">("")
  const t = useT()
  const filters: FeedFilter = { unread, ...(kind ? { kind } : {}), ...(source ? { source } : {}) }
  return <FeedReader key={JSON.stringify(filters)} client={client} filters={filters} controls={<div className="flex min-w-0 flex-wrap items-center gap-2" data-feed-filters>
    <Segmented value={unread ? "unread" : "all"} onChange={(v) => setUnread(v === "unread")} options={[["all", t("views:feed.all")], ["unread", t("views:feed.unread")]]} />
    <Select aria-label={t("views:feed.kind")} className="w-36 max-sm:flex-1" value={kind} onChange={(v) => setKind(v as FeedKind | "")} options={[{ value: "", label: t("views:feed.allKinds") }, { value: "report", label: t("views:feed.report") }, { value: "completion", label: t("views:feed.completion") }]} />
    <Select aria-label={t("views:feed.source")} className="w-40 max-sm:flex-1" value={source} onChange={(v) => setSource(v as FeedSource | "")} options={[{ value: "", label: t("views:feed.allSources") }, { value: "flow", label: t("views:feed.flow") }, { value: "notification", label: t("views:feed.notification") }]} />
  </div>} />
}

function FeedReader({ client, filters, controls }: { client: FeedClient; filters: FeedFilter; controls: React.ReactNode }) {
  const t = useT()
  const [data, setData] = useState<FeedPage>()
  const [busy, setBusy] = useState<"refresh" | "more" | "read" | null>("refresh")
  const [error, setError] = useState<{ error: unknown; retry: () => void }>()
  const [checkError, setCheckError] = useState<unknown>()
  const [newContent, setNewContent] = useState(false)
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [confirm, setConfirm] = useState(false)
  const [status, setStatus] = useState("")
  const alive = useRef(true), operation = useRef(0), pending = useRef(false), latest = useRef(data)
  const scroll = useRef<HTMLDivElement>(null)
  latest.current = data
  const controllers = useRef(new Set<AbortController>())
  const request = async <T,>(load: (signal: AbortSignal) => Promise<T>) => {
    const controller = new AbortController(); controllers.current.add(controller)
    try { return await load(controller.signal) } finally { controllers.current.delete(controller) }
  }
  const load = async (more = false) => {
    if (pending.current) return
    const before = latest.current
    if (more && !before?.next_cursor) return
    pending.current = true
    const n = ++operation.current
    setBusy(more ? "more" : "refresh"); setError(undefined)
    try {
      const page = await request((signal) => client.list(filters, more ? before!.next_cursor! : undefined, 50, signal))
      if (!alive.current || n !== operation.current) return
      setData(more ? { ...page, snapshot: before!.snapshot, entries: appendFeed(before!.entries, page.entries) } : page)
      if (!more) { setSelected(new Set()); setNewContent(false); setCheckError(undefined); setConfirm(false); scroll.current?.scrollTo({ top: 0 }) }
    } catch (error) { if (alive.current && n === operation.current) setError({ error, retry: () => void load(error instanceof HubError && error.status === 400 ? false : more) }) }
    finally { if (alive.current && n === operation.current) { pending.current = false; setBusy(null) } }
  }
  const mark = async (body: FeedRead) => {
    if (pending.current) return
    pending.current = true
    const n = ++operation.current
    setBusy("read"); setError(undefined)
    try {
      const result = await request((signal) => client.read(body, signal))
      if (!alive.current || n !== operation.current) return
      const ids = "ids" in body ? new Set(body.ids) : new Set(latest.current?.entries.map((entry) => entry.id))
      const at = Date.now()
      setData((old) => old ? { ...old, unread_count: Math.max(0, old.unread_count - result.updated), entries: old.entries.map((entry) => ids.has(entry.id) && entry.read_at === null ? { ...entry, read_at: at } : entry) } : old)
      setSelected(new Set()); setConfirm(false); setStatus(t("views:feed.marked", { count: result.updated }))
    } catch (error) { if (alive.current && n === operation.current) setError({ error, retry: () => { if (error instanceof HubError && error.status === 400 && "snapshot" in body) void load(); else void mark(body) } }) }
    finally { if (alive.current && n === operation.current) { pending.current = false; setBusy(null) } }
  }
  useEffect(() => {
    alive.current = true; void load()
    // Private feed content is never subscribed to workspace SSE. Probe only while visible/focused;
    // keep the current snapshot and scroll position until the reader explicitly refreshes.
    let checking = false
    const check = async () => {
      const before = latest.current
      if (!before || pending.current || checking || document.visibilityState !== "visible" || !document.hasFocus()) return
      checking = true
      const n = operation.current
      try {
        const page = await request((signal) => client.list(filters, undefined, 1, signal))
        if (!alive.current || operation.current !== n) return
        setCheckError(undefined)
        setNewContent((hasNew) => hasNew || !!(page.entries[0] && page.entries[0].seq > (before.entries[0]?.seq ?? 0)))
        setData((old) => old ? { ...old, unread_count: page.unread_count } : old)
      } catch (error) { if (alive.current && operation.current === n) setCheckError(error) }
      finally { checking = false }
    }
    const timer = window.setInterval(() => void check(), 30_000)
    const onFocus = () => void check()
    window.addEventListener("focus", onFocus); document.addEventListener("visibilitychange", onFocus)
    return () => { alive.current = false; operation.current++; pending.current = false; clearInterval(timer); window.removeEventListener("focus", onFocus); document.removeEventListener("visibilitychange", onFocus); controllers.current.forEach((controller) => controller.abort()); controllers.current.clear() }
  }, [])
  const entries = data?.entries ?? [], selectable = entries.filter((entry) => entry.read_at === null)
  const toggle = (id: string, checked: boolean) => setSelected((old) => { const next = new Set(old); if (checked && next.size < 100) next.add(id); else if (!checked) next.delete(id); return next })
  const count = data?.unread_count
  return <Page title={t("core:nav.feed")} sub={t("views:feed.sub")} max={920} scrollRef={scroll} actions={<Button size="sm" variant={newContent ? "outline" : "ghost"} aria-label={t("common:action.refresh")} disabled={busy !== null} onClick={() => void load()} data-feed-refresh data-feed-new={newContent ? "" : undefined}><RefreshCwIcon className="size-4" />{newContent && <span role="status">{t("views:feed.newContent")}</span>}</Button>}>
    <div data-feed className="min-w-0">
      {controls}
      <div className="mt-4 flex min-h-10 flex-wrap items-center gap-x-3 gap-y-2 border-b border-border pb-3 text-sm">
        <Checkbox aria-label={t("views:feed.selectLoaded")} checked={selectable.length > 0 && selected.size === Math.min(100, selectable.length)} indeterminate={selected.size > 0 && selected.size < Math.min(100, selectable.length)} disabled={!selectable.length || busy !== null} onChange={(e) => setSelected(e.currentTarget.checked ? new Set(selectable.slice(0, 100).map((entry) => entry.id)) : new Set())} data-feed-select-all />
        <span className="mr-auto text-muted-foreground" data-feed-count>{count === undefined ? t("common:state.loading") : t("views:feed.count", { count })}</span>
        {selected.size > 0 && <Button size="sm" variant="outline" disabled={busy !== null} onClick={() => void mark({ ids: [...selected] })} data-feed-read-selected>{t("views:feed.readSelected", { count: selected.size })}</Button>}
        <Button size="sm" variant="ghost" disabled={!data || !count || busy !== null} onClick={() => setConfirm(true)} data-feed-read-all>{t("views:feed.readAll")}</Button>
      </div>
      {(selectable.length > 100 || selected.size > 0) && <p className="pt-2 text-xs text-muted-foreground" data-feed-selection-limit>{t("views:feed.selectionLimit")}</p>}
      {confirm && data && <div className="my-3 rounded-lg border border-border bg-muted/30 p-3 text-sm" data-feed-confirm>
        <p>{t("views:feed.snapshotNote")}</p><div className="mt-3 flex gap-2"><Button size="sm" disabled={busy !== null} onClick={() => void mark({ snapshot: data.snapshot, ...filters })} data-feed-confirm-read>{t("views:feed.confirmRead")}</Button><Button size="sm" variant="ghost" disabled={busy !== null} onClick={() => setConfirm(false)}>{t("common:action.cancel")}</Button></div>
      </div>}
      {checkError != null && <div className="my-3 text-sm text-muted-foreground" role="status">{t("views:feed.checkFailed")} <Button size="sm" variant="ghost" disabled={busy !== null} onClick={() => void load()}>{t("common:action.retry")}</Button></div>}
      {error && <FeedError error={error.error} retry={error.retry} disabled={busy !== null} />}
      <span role="status" className="sr-only" data-feed-status>{status}</span>
      {busy === "refresh" && !data ? <Loading /> : data && !entries.length ? <EmptyState bare title={t(filters.unread ? "views:feed.emptyUnread" : "views:feed.empty")} text={t("views:feed.emptySub")} /> : null}
      <div data-feed-list>{entries.map((entry, index) => {
        const day = new Date(entry.published_at).toLocaleDateString(lang(), { year: "numeric", month: "long", day: "numeric" })
        const previous = index ? new Date(entries[index - 1].published_at).toLocaleDateString(lang(), { year: "numeric", month: "long", day: "numeric" }) : null
        return <div key={entry.id}>{day !== previous && <h2 className="pb-1 pt-6 text-xs font-medium text-muted-foreground" data-feed-day>{day}</h2>}<FeedRow client={client} entry={entry} checked={selected.has(entry.id)} disabled={busy !== null} selectionDisabled={selected.size >= 100 && !selected.has(entry.id)} onSelect={(value) => toggle(entry.id, value)} onRead={() => void mark({ ids: [entry.id] })} /></div>
      })}</div>
      {data?.next_cursor && <div className="flex justify-center py-6"><Button variant="outline" disabled={busy !== null} onClick={() => void load(true)} data-feed-more>{t(busy === "more" ? "common:state.loading" : "views:feed.more")}</Button></div>}
    </div>
  </Page>
}

function FeedError({ error, retry, disabled }: { error: unknown; retry: () => void; disabled?: boolean }) {
  const t = useT()
  const reset = error instanceof HubError && error.status === 400
  return <div role="alert" className="my-3 flex flex-wrap items-center gap-3 rounded-lg border border-destructive/25 p-3 text-sm"><p className="min-w-0 flex-1 [overflow-wrap:anywhere]">{reset ? t("views:feed.expired") : t("views:feed.failed", { error: errText(error) })}</p><Button size="sm" variant="outline" onClick={retry} disabled={disabled}>{t(reset ? "views:feed.reset" : "common:action.retry")}</Button></div>
}

function FeedRow({ client, entry, checked, disabled, selectionDisabled, onSelect, onRead }: { client: FeedClient; entry: FeedEntry; checked: boolean; disabled: boolean; selectionDisabled: boolean; onSelect: (value: boolean) => void; onRead: () => void }) {
  const t = useT()
  const [open, setOpen] = useState(false), [detail, setDetail] = useState<FeedEntry>(), [loading, setLoading] = useState(false), [error, setError] = useState<unknown>()
  const seq = useRef(0), ctl = useRef<AbortController | null>(null)
  const load = async () => {
    ctl.current?.abort(); const controller = new AbortController(); ctl.current = controller
    const n = ++seq.current; setLoading(true); setError(undefined)
    try { const value = await client.detail(entry.id, controller.signal); if (seq.current === n) setDetail(value) }
    catch (error) { if (seq.current === n) setError(error) }
    finally { if (seq.current === n) setLoading(false) }
  }
  useEffect(() => () => { seq.current++; ctl.current?.abort() }, [])
  const title = entry.title.trim() || t("views:feed.untitled")
  const body = detail?.body, purged = entry.purged || detail?.purged
  const contentId = `feed-${entry.id}`
  const links = [...(body?.references ?? []), ...(body?.actions ?? [])].map(([label, url]) => {
    const href = feedHref(url)
    const item = href?.match(/^#\/items\/([^/?#]+)$/)?.[1]
    let display = label
    try { if (item && decodeURIComponent(item) === label) display = t("views:feed.viewItem") } catch { /* Preserve an explicit source label if the URL is malformed. */ }
    return { label: display, href }
  }).filter((link) => link.href)
  // The verified rule route redirects schedule sources to their schedules page; it also accepts ?run=.
  const flowHref = entry.source.type === "flow" ? `#/auto/rule/${encodeURIComponent(entry.source.id)}${entry.source.run_id ? `?run=${encodeURIComponent(entry.source.run_id)}` : ""}` : null
  return <article className="min-w-0 border-b border-border/70 py-3" data-feed-entry={entry.id} data-feed-read={entry.read_at !== null}>
    <div className="flex items-start gap-3">
      <div className="shrink-0 pt-1"><Checkbox aria-label={t("views:feed.select", { title })} checked={checked} disabled={disabled || selectionDisabled || entry.read_at !== null} onChange={(e) => onSelect(e.currentTarget.checked)} /></div>
      <div className="min-w-0 flex-1">
        <button className="group w-full min-w-0 text-left focus-visible:outline-primary" aria-expanded={open} aria-controls={contentId} onClick={() => { setOpen(!open); if (!open && !detail && !loading) void load() }} data-feed-expand>
          <span className="flex items-start gap-2"><span className={`mt-1.5 size-1.5 shrink-0 rounded-full ${entry.read_at === null ? "bg-primary" : "bg-transparent"}`} aria-label={entry.read_at === null ? t("views:feed.unread") : undefined} /><span className="min-w-0 flex-1 break-words text-sm leading-6 font-medium [overflow-wrap:anywhere]">{title}</span>{open ? <ChevronUpIcon className="mt-1 size-4 shrink-0 text-muted-foreground" /> : <ChevronDownIcon className="mt-1 size-4 shrink-0 text-muted-foreground" />}</span>
          {!open && <p className="ml-3.5 mt-0.5 line-clamp-2 text-sm leading-5 text-muted-foreground [overflow-wrap:anywhere]">{purged ? t("views:feed.purged") : entry.excerpt}</p>}
        </button>
        <div className="ml-3.5 mt-1 flex flex-wrap gap-x-2 gap-y-1 text-xs text-muted-foreground"><span>{t(entry.kind === "report" ? "views:feed.report" : "views:feed.completion")}</span><span aria-hidden="true">·</span><span>{t(entry.source.type === "flow" ? "views:feed.flow" : "views:feed.notification")}</span><time dateTime={new Date(entry.published_at).toISOString()}>{new Date(entry.published_at).toLocaleTimeString(lang(), { hour: "2-digit", minute: "2-digit" })}</time></div>
        {open && <div id={contentId} className="ml-3.5 mt-4 min-w-0" data-feed-body>
          {loading && <Loading />}
          {error != null && (error instanceof HubError && error.status === 404 ? <p role="alert" className="text-sm text-muted-foreground">{t("views:feed.unavailable")}</p> : <FeedError error={error} retry={() => void load()} />)}
          {purged ? <p className="text-sm text-muted-foreground">{t("views:feed.purged")}</p> : body ? <>
            {body.markdown && <div className="min-w-0 text-sm leading-7 [overflow-wrap:anywhere] [&_pre]:max-w-full [&_pre]:overflow-x-auto [&_table]:block [&_table]:max-w-full [&_table]:overflow-x-auto"><Markdown text={body.markdown} /></div>}
            {!!body.fields.length && <dl className="my-4 grid grid-cols-[minmax(0,1fr)_minmax(0,2fr)] gap-x-4 gap-y-2 text-sm [overflow-wrap:anywhere]">{body.fields.map(([key, value], i) => <div key={i} className="contents"><dt className="text-muted-foreground">{key}</dt><dd>{value}</dd></div>)}</dl>}
          </> : detail && !loading && !error ? <p className="text-sm text-muted-foreground">{t("views:feed.noBody")}</p> : null}
          <div className="mt-4 flex flex-wrap items-center gap-x-4 gap-y-2 text-xs">
            {flowHref && <a href={flowHref} className="inline-flex items-center gap-1 text-primary underline-offset-4 hover:underline" data-feed-source>{t("views:feed.viewSource")}<ExternalLinkIcon className="size-3" /></a>}
            {links.map((link, index) => <a key={index} href={link.href!} {...(link.href!.startsWith("http") ? { target: "_blank", rel: "noopener noreferrer" } : {})} className="max-w-full text-primary underline-offset-4 [overflow-wrap:anywhere] hover:underline">{link.label || t("views:feed.viewSource")}</a>)}
            {!flowHref && !links.length && <span className="text-muted-foreground">{t("views:feed.noSource")}</span>}
            {entry.read_at === null && <Button size="sm" variant="outline" disabled={disabled} onClick={onRead} data-feed-read-one>{t("views:feed.markRead")}</Button>}
          </div>
        </div>}
      </div>
    </div>
  </article>
}
