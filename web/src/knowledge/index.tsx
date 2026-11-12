/* 知识 (knowledge.md §9): search, long-term memories, knowledge entries, notes and documents, sources, and
 * candidates waiting for you. The four kinds of record (ST-41): 事项记录 and 会话记录 are documents kept as
 * they are; 长期记忆 (preference, rule) and 知识条目 (fact, event) are what Steve distils from them, one tab each.
 * Every list is the Hub's; on a Hub without /api/kb the whole page is the "还没有接入" placeholder. */
import { useHubEvents } from "@/store"
import { Fragment, useCallback, useEffect, useRef, useState, type ReactNode } from "react"
import { cn } from "cn"
import { FilePlusIcon, FolderPlusIcon, MoreHorizontalIcon, PlusIcon, RefreshCwIcon, SearchIcon, TrashIcon, UploadIcon, XIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Switch } from "@/components/ui/switch"
import { Spinner } from "@/components/ui/spinner"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Page, SectionLabel, Group, Row, Segmented, Chips, Empty, StatusDot, go } from "@/ui"
import { toast, tm } from "@/store"
import { useUploads } from "@/attach"
import { kb, levelLabel, visLabel, memKind, recordOf, LONG_TERM, MEM_KINDS, SOURCE_KIND, LEVEL_DOT, type Candidate, type ExpiredCandidate, type DocBrief, type Hit, type KbSource, type Mem } from "@/api/knowledge"
import { KbGate, LevelSel, VisSel, errMsg } from "@/knowledge/fields"
import { DocView, NoteView } from "@/knowledge/doc"
import { t as tt, useT } from "@/i18n"

/* tab values are ids (also data-seg / data-kb-tab); the labels come from knowledge:tab.* */
type Tab = "搜索" | "长期记忆" | "知识条目" | "笔记与文档" | "来源" | "待确认"
type RecordName = "长期记忆" | "知识条目"
let tab: Tab = "长期记忆", query = "", tabHash = ""

/** `#/knowledge`, `#/knowledge?mem=…`, `#/knowledge/doc/:id?seq=`, `#/knowledge/note/:id` */
export function KnowledgeView({ sub, id }: { sub?: string; id?: string }) {
  const [docId] = (id ?? "").split("?")
  return <KbGate>{sub === "doc" && docId ? <DocView id={docId} /> : sub === "note" && docId ? <NoteView id={docId} /> : <KnowledgeHome />}</KbGate>
}

const hashParam = (k: string) => new URLSearchParams(location.hash.split("?")[1] ?? "").get(k)

function KnowledgeHome() {
  const t = useT()
  const [, f] = useState(0), r = () => f((x) => x + 1)
  const focusMem = hashParam("mem")
  if (focusMem && tab !== "长期记忆" && tab !== "知识条目") tab = "长期记忆"
  /* 待处理's line “N 条记忆待确认” opens the batch page (once per visit of that link) */
  if (hashParam("tab") === "pending" && tabHash !== location.hash) { tab = "待确认"; tabHash = location.hash }
  else if (hashParam("tab") !== "pending") tabHash = ""
  const [pend, setPend] = useState(0)
  const loadPend = useCallback(() => { kb.candidates().then((xs) => setPend(xs.length), () => {}) }, [])
  useEffect(loadPend, [loadPend])
  useHubEvents(["mm", "kb"], loadPend)
  const setTab = (x: Tab) => { tab = x; r() }
  const opts = [["搜索", t("knowledge:tab.search")], ["长期记忆", t("knowledge:tab.mems")], ["知识条目", t("knowledge:tab.entries")], ["笔记与文档", t("knowledge:tab.docs")], ["来源", t("knowledge:tab.sources")],
    ["待确认", <span key="p" className="inline-flex items-center gap-1">{t("knowledge:tab.pending")}{pend > 0 && <span className="rounded-full bg-attention px-1.5 text-[10px] font-semibold text-white" data-pending-count>{pend}</span>}</span>, t("knowledge:tab.pending")]] as const
  return (
    <Page title={t("knowledge:page.title")} sub={t("knowledge:page.sub")} max={760}>
      <div className="flex flex-col gap-5">
        <SearchBox onQuery={(q) => { query = q; if (q && tab !== "搜索") tab = "搜索"; r() }} />
        <div className="-mx-1 overflow-x-auto px-1 pb-0.5 [scrollbar-width:none]"><Segmented value={tab} onChange={setTab} options={opts} /></div>
        <div data-kb-tab={tab}>
          {tab === "搜索" && <SearchTab q={query} />}
          {(tab === "长期记忆" || tab === "知识条目") && <MemsTab key={tab} record={tab} focus={focusMem} onRecord={setTab} />}
          {tab === "笔记与文档" && <DocsTab />}
          {tab === "来源" && <SourcesTab />}
          {tab === "待确认" && <CandidatesTab onChange={setPend} />}
        </div>
      </div>
    </Page>
  )
}

function SearchBox({ onQuery }: { onQuery: (q: string) => void }) {
  const t = useT()
  const [v, setV] = useState(query), timer = useRef<number>(0)
  const set = (x: string) => { setV(x); clearTimeout(timer.current); timer.current = window.setTimeout(() => onQuery(x.trim()), 250) }
  return (
    <div role="search" className="flex items-center gap-2 rounded-xl border bg-card px-3 focus-within:border-blue/60">
      <SearchIcon className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.75} />
      <input type="search" aria-label={t("knowledge:search.aria")} value={v} onChange={(e) => set(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter") { clearTimeout(timer.current); onQuery(v.trim()) } }}
        placeholder={t("knowledge:search.placeholder")} maxLength={200} className="h-10 min-w-0 flex-1 bg-transparent text-[14px] outline-none placeholder:text-tertiary [&::-webkit-search-cancel-button]:hidden" data-kb-search />
      {v && <Button variant="ghost" size="icon-xs" aria-label={t("knowledge:search.clear")} className="text-muted-foreground" onClick={() => { setV(""); onQuery("") }}><XIcon /></Button>}
    </div>
  )
}

/* ───────── 搜索 ───────── */
/** wraps each query term found in `text` in <mark>; done here, the Hub returns plain snippets */
function Highlight({ text, q }: { text: string; q: string }) {
  const terms = q.split(/\s+/).filter(Boolean).map((x) => x.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"))
  if (!terms.length) return <>{text}</>
  const re = new RegExp(`(${terms.join("|")})`, "gi")
  return <>{text.split(re).map((p, n) => (n % 2 ? <mark key={n} className="rounded-sm bg-attention/20 px-px text-foreground">{p}</mark> : <Fragment key={n}>{p}</Fragment>))}</>
}
const LevelTag = ({ l }: { l: string }) => <span className="inline-flex shrink-0 items-center gap-1 type-footnote text-muted-foreground" data-level={l}><StatusDot s={LEVEL_DOT[l] ?? "idle"} />{levelLabel(l)}</span>

function SearchTab({ q }: { q: string }) {
  const t = useT()
  const [res, setRes] = useState<{ q: string; hits: Hit[]; ms: number; down: boolean } | null>(null), [err, setErr] = useState("")
  useEffect(() => {
    if (!q) { setRes(null); return }
    let on = true; setErr("")
    kb.search(q).then((x) => on && setRes({ q, hits: x.hits, ms: x.took_ms, down: !!x.semantic_down }), (e) => on && setErr(errMsg(e)))
    return () => { on = false }
  }, [q])
  if (!q) return <Empty icon={<SearchIcon />} title={t("knowledge:search.idleTitle")} sub={t("knowledge:search.idleSub")} />
  if (err) return <Empty title={t("knowledge:search.failed")} sub={err} />
  if (!res || res.q !== q) return <div className="flex justify-center py-12" role="status" aria-label={t("knowledge:search.searching")}><Spinner className="size-5 text-muted-foreground" /></div>
  /* the meaning search was down: the hits are from keywords only, and the page says so */
  const down = res.down && <p className="px-1 pb-2 type-footnote text-muted-foreground" role="note" data-kb-keywords-only>{t("knowledge:search.keywordsOnly")}</p>
  if (!res.hits.length) return <>{down}<Empty title={t("knowledge:search.noneTitle")} sub={t("knowledge:search.noneSub", { q })} /></>
  return (
    <section aria-label={t("knowledge:search.results")}>{down}<SectionLabel>{t("knowledge:search.count", { count: res.hits.length, ms: res.ms })}</SectionLabel>
      <Group>{res.hits.map((h) => (
        <Row key={h.type + h.id + h.seq} onClick={() => go(h.href)} data-hit={h.id}
          title={<span className="flex min-w-0 items-baseline gap-2"><span className="min-w-0 truncate"><Highlight text={h.title} q={q} />{h.heading ? <span className="text-muted-foreground"> · <Highlight text={h.heading} q={q} /></span> : null}</span></span>}
          sub={<span className="flex flex-col gap-1"><span className="line-clamp-3 type-callout text-foreground/80"><Highlight text={h.text} q={q} /></span><span className="truncate">{h.label}</span></span>}>
          <LevelTag l={h.level} />
        </Row>))}</Group>
    </section>
  )
}

/* ───────── 长期记忆 / 知识条目 ───────── */
/* the kind a typed entry gets, and where its texts live: knowledge:record.<key>.* */
const RECORD: Record<RecordName, { kind: string; key: "mem" | "entry" }> = {
  长期记忆: { kind: "rule", key: "mem" },
  知识条目: { kind: "fact", key: "entry" },
}
/** the tab a memory of `kind` lives on (an id; `recordOf` is its label in the UI language) */
const recordTab = (kind: string): RecordName => (LONG_TERM.includes(kind) ? "长期记忆" : "知识条目")
const aboutKey = (m: Mem) => m.about.map((a) => a.type + ":" + a.id).sort().join(",")
const aboutName = (a: Mem["about"][number]) => a.name ?? (a.type === "party" ? tt("knowledge:mems.aboutParty", { id: a.id }) : a.type === "item" ? tt("knowledge:mems.aboutItem", { id: a.id }) : a.id)
const aboutLabel = (m: Mem) => (m.about.length ? tt("knowledge:mems.aboutLabel", { names: m.about.map(aboutName).join(tt("knowledge:listSep")) }) : tt("knowledge:mems.general"))

function MemsTab({ record, focus, onRecord }: { record: RecordName; focus: string | null; onRecord: (r: RecordName) => void }) {
  const t = useT()
  const R = RECORD[record], k = R.key
  /* "" is every kind; the chips show kind labels, with the "all" label first */
  const [xs, setXs] = useState<Mem[] | null>(null), [err, setErr] = useState(""), [kind, setKind] = useState(""), [v, setV] = useState(""), [busy, setBusy] = useState(false)
  const load = useCallback(() => kb.mems().then(setXs, (e) => setErr(errMsg(e))), [])
  useEffect(() => { void load() }, [load])
  useHubEvents(["mm"], () => void load())
  /* a linked memory opens on the tab it belongs to */
  const focused = focus ? xs?.find((m) => m.id === focus) : undefined
  useEffect(() => { if (focused && recordTab(focused.kind) !== record) onRecord(recordTab(focused.kind)) }, [focused, record, onRecord])
  useEffect(() => { if (focus && xs) document.querySelector(`[data-mem="${CSS.escape(focus)}"]`)?.scrollIntoView({ block: "center" }) }, [focus, xs])
  const add = async () => {
    const text = v.trim(); if (!text || busy) return
    setBusy(true)
    try { await kb.addMem({ text, kind: R.kind }); setV(""); toast(t("knowledge:mems.added")); await load() } catch (e) { toast(errMsg(e)) } finally { setBusy(false) }
  }
  const patch = async (m: Mem, p: Parameters<typeof kb.patchMem>[1]) => { try { const n = await kb.patchMem(m.id, p); setXs((a) => a?.map((x) => (x.id === m.id ? { ...x, ...n } : x)) ?? null) } catch (e) { toast(errMsg(e)) } }
  const remove = async (m: Mem) => { try { await kb.deleteMem(m.id); setXs((a) => a?.filter((x) => x.id !== m.id) ?? null); toast(t("knowledge:mems.deleted")) } catch (e) { toast(errMsg(e)) } }
  const mine = (xs ?? []).filter((m) => recordTab(m.kind) === record)
  const all = t("knowledge:all")
  const kinds = [all, ...MEM_KINDS.filter(([c]) => mine.some((m) => m.kind === c)).map(([, l]) => l)]
  const shown = mine.filter((m) => !kind || memKind(m.kind) === kind)
  const groups = new Map<string, Mem[]>(); for (const m of shown) { const g = aboutKey(m); groups.set(g, [...(groups.get(g) ?? []), m]) }
  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center gap-2 rounded-xl border bg-card py-1 pl-3 pr-1 focus-within:border-blue/60">
        <input aria-label={t(`knowledge:record.${k}.addAria`)} value={v} onChange={(e) => setV(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && !e.nativeEvent.isComposing) void add() }} maxLength={2000}
          placeholder={t(`knowledge:record.${k}.hint`)} className="h-8 min-w-0 flex-1 bg-transparent text-[14px] outline-none placeholder:text-tertiary" data-mem-input />
        <Button size="sm" disabled={!v.trim() || busy} onClick={() => void add()} data-mem-add>{busy ? <Spinner className="size-3.5" /> : <PlusIcon />}{t("knowledge:mems.add")}</Button>
      </div>
      {kinds.length > 2 && <Chips value={kind && kinds.includes(kind) ? kind : all} onChange={(x) => setKind(x === all ? "" : x)} options={kinds} />}
      {err ? <Empty title={t(`knowledge:record.${k}.failed`)} sub={err} /> : !xs ? <Loading /> : !shown.length ? <Empty title={t(`knowledge:record.${k}.none`)} sub={t(`knowledge:record.${k}.empty`)} /> :
        [...groups.entries()].map(([g, ms]) => (
          <section key={g}><SectionLabel>{aboutLabel(ms[0])} · {ms.length}</SectionLabel>
            <Group>{ms.map((m) => (
              <Row key={m.id} data-mem={m.id} className={cn(STACK, m.id === focus && "bg-blue/5", m.status === "stale" && "opacity-60")}
                title={<span className="inline-flex flex-wrap items-center gap-2">{m.text}{m.status === "candidate" && <Badge variant="outline" className="font-normal text-attention">{t("knowledge:mems.pending")}</Badge>}{m.status === "stale" && <Badge variant="outline" className="font-normal text-muted-foreground">{t("knowledge:mems.stale")}</Badge>}</span>}
                sub={t("knowledge:mems.sub", { kind: memKind(m.kind), source: m.source_label || t("knowledge:mems.youWrote") })}>
                <span className="flex flex-wrap items-center justify-end gap-0.5 max-sm:justify-start">
                  <LevelSel v={m.level} on={(l) => l && void patch(m, { level: l })} testId={"mem-level-" + m.id} />
                  <VisSel v={m.visibility} on={(x) => void patch(m, { visibility: x })} testId={"mem-vis-" + m.id} />
                  <Button variant="ghost" size="icon-xs" aria-label={t("knowledge:mems.deleteAria", { text: m.text })} className="text-muted-foreground" onClick={() => void remove(m)} data-mem-del><XIcon /></Button>
                </span>
              </Row>))}</Group>
          </section>))}
      <div className="-mt-2 px-1 type-footnote text-muted-foreground">{t("knowledge:mems.footer", { note: t(`knowledge:record.${k}.note`) })}</div>
    </div>
  )
}
/** on a phone the row's controls go on their own line under the text */
const STACK = "max-sm:[&>[data-slot=item-actions]]:w-full max-sm:[&>[data-slot=item-actions]]:justify-start max-sm:[&>[data-slot=item-actions]]:-ml-2"
const Loading = () => { const t = useT(); return <div className="flex justify-center py-12" role="status" aria-label={t("knowledge:page.loading")}><Spinner className="size-5 text-muted-foreground" /></div> }

/* ───────── 笔记与文档 ───────── */
export const docHref = (d: { id: string; source: { kind: string } }) => (d.source.kind === "notes" ? "#/knowledge/note/" : "#/knowledge/doc/") + d.id
function DocsTab() {
  const t = useT()
  /* src "" is every source; the chips show source names, with the "all" label first */
  const [xs, setXs] = useState<DocBrief[] | null>(null), [err, setErr] = useState(""), [src, setSrc] = useState(""), [newNote, setNewNote] = useState(false)
  const loadDocs = useCallback(() => kb.docs().then(setXs, (e) => setErr(errMsg(e))), [])
  useEffect(() => { void loadDocs() }, [loadDocs])
  useHubEvents(["kb"], () => void loadDocs())
  const all = t("knowledge:all")
  const names = [all, ...new Set((xs ?? []).map((d) => d.source.name))]
  const shown = (xs ?? []).filter((d) => !src || d.source.name === src)
  return (
    <div className="flex flex-col gap-5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        {names.length > 2 ? <Chips value={src && names.includes(src) ? src : all} onChange={(x) => setSrc(x === all ? "" : x)} options={names} /> : <span />}
        <Button size="sm" onClick={() => setNewNote(true)} data-new-note><FilePlusIcon />{t("knowledge:docs.newNote")}</Button>
      </div>
      {err ? <Empty title={t("knowledge:docs.failed")} sub={err} /> : !xs ? <Loading /> : !shown.length ? <Empty title={t("knowledge:docs.noneTitle")} sub={t("knowledge:docs.noneSub")} /> :
        <Group>{shown.map((d) => (
          <Row key={d.id} onClick={() => go(docHref(d))} data-doc={d.id}
            title={<span className="truncate">{d.title}</span>}
            sub={<span className="flex flex-wrap gap-x-1.5"><span>{d.source.name}</span>{d.source.kind !== "notes" && d.ref && <span className="truncate">· {d.ref}</span>}<span>· {visLabel(d.visibility)}</span><span>· {tm(d.updated)}</span>{d.status !== "active" && <span className="text-attention">· {d.status === "error" ? t("knowledge:docs.error") : d.status}</span>}</span>}>
            <LevelTag l={d.level} />
          </Row>))}</Group>}
      <NewNoteDialog open={newNote} onClose={() => setNewNote(false)} />
    </div>
  )
}

function NewNoteDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const t = useT()
  const [title, setTitle] = useState(""), [body, setBody] = useState(""), [busy, setBusy] = useState(false)
  const save = async () => {
    if (!title.trim() || busy) return
    setBusy(true)
    try { const d = await kb.newNote({ title: title.trim(), body }); toast(t("knowledge:newNote.saved")); onClose(); go("#/knowledge/note/" + d.id) } catch (e) { toast(errMsg(e)) } finally { setBusy(false) }
  }
  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[560px]">
        <DialogHeader><DialogTitle className="type-headline">{t("knowledge:newNote.title")}</DialogTitle><DialogDescription className="type-callout">{t("knowledge:newNote.desc")}</DialogDescription></DialogHeader>
        <FieldGroup className="gap-4">
          <Field><FieldLabel htmlFor="kb-note-title">{t("knowledge:newNote.titleLabel")}</FieldLabel><Input id="kb-note-title" autoFocus maxLength={120} value={title} onChange={(e) => setTitle(e.target.value)} placeholder={t("knowledge:newNote.titlePlaceholder")} data-note-title /></Field>
          <Field><FieldLabel htmlFor="kb-note-body">{t("knowledge:newNote.bodyLabel")}</FieldLabel><Textarea id="kb-note-body" className="min-h-40 font-mono text-[13px]" value={body} onChange={(e) => setBody(e.target.value)} placeholder={t("knowledge:newNote.bodyPlaceholder")} data-note-body
            onKeyDown={(e) => { if ((e.metaKey || e.ctrlKey) && e.key === "Enter") void save() }} /></Field>
        </FieldGroup>
        <DialogFooter className="gap-2"><Button variant="ghost" onClick={onClose}>{t("common:action.cancel")}</Button><Button disabled={!title.trim() || busy} onClick={() => void save()} data-note-save>{busy && <Spinner className="size-3.5" />}{t("common:action.save")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/* ───────── 来源 ───────── */
const statsLine = (s: KbSource) => {
  const x = s.stats ?? {}, parts = [tt("knowledge:sources.docs", { count: x.docs ?? 0 }), tt("knowledge:sources.chunks", { count: x.chunks ?? 0 })]
  if (x.skipped) parts.push(tt("knowledge:sources.skipped", { count: x.skipped }))
  /* a scan that could not see everything deletes nothing */
  if (x.kept) parts.push(tt("knowledge:sources.kept", { count: x.kept })); else if (x.incomplete?.length) parts.push(tt("knowledge:sources.incomplete"))
  /* built-in sources follow their objects as they change; only folders are scanned */
  if (s.status === "syncing") parts.push(tt("knowledge:sources.syncing")); else if (s.last_sync) parts.push(tt("knowledge:sources.lastSync", { time: tm(s.last_sync) })); else if (!s.builtin) parts.push(tt("knowledge:sources.neverSynced"))
  return parts.join(" · ")
}
function SourcesTab() {
  const t = useT()
  const [xs, setXs] = useState<KbSource[] | null>(null), [err, setErr] = useState(""), [folder, setFolder] = useState(false)
  const load = useCallback(() => kb.sources().then(setXs, (e) => setErr(errMsg(e))), [])
  useEffect(() => { void load() }, [load])
  useHubEvents(["kb"], () => void load())
  /* a scan runs in the background: poll while one is going */
  const syncing = xs?.some((s) => s.status === "syncing")
  useEffect(() => { if (!syncing) return; const iv = setInterval(() => void load(), 2000); return () => clearInterval(iv) }, [syncing, load])
  const patch = async (s: KbSource, p: Parameters<typeof kb.patchSource>[1], msg?: string) => { try { const n = await kb.patchSource(s.id, p); setXs((a) => a?.map((x) => (x.id === s.id ? n : x)) ?? null); if (msg) toast(msg) } catch (e) { toast(errMsg(e)) } }
  const sync = async (s: KbSource) => { try { await kb.sync(s.id); toast(t("knowledge:sources.syncStarted")); setTimeout(() => void load(), 300) } catch (e) { toast(errMsg(e)) } }
  const remove = async (s: KbSource) => { try { await kb.deleteSource(s.id); toast(t("knowledge:sources.removed")); void load() } catch (e) { toast(errMsg(e)) } }
  return (
    <div className="flex flex-col gap-5">
      <div className="flex flex-wrap justify-end gap-2"><AddFiles onDone={() => void load()} /><Button size="sm" onClick={() => setFolder(true)} data-add-folder><FolderPlusIcon />{t("knowledge:sources.addFolder")}</Button></div>
      {err ? <Empty title={t("knowledge:sources.failed")} sub={err} /> : !xs ? <Loading /> :
        <Group>{xs.map((s) => (
          <Row key={s.id} data-source={s.id} data-source-kind={s.kind} className={cn(STACK, s.paused && "opacity-70")}
            title={<span className="inline-flex flex-wrap items-center gap-2">{s.name}<span className="type-footnote text-muted-foreground">{SOURCE_KIND[s.kind] ?? s.kind}</span>{s.status === "error" && <Badge variant="outline" className="font-normal text-destructive">{t("knowledge:sources.error")}</Badge>}</span>}
            sub={<span className="flex flex-col"><span data-source-stats>{statsLine(s)}</span>{s.path && <span className="truncate font-mono text-[11.5px]">{s.path.replace(/^\/Users\/[^/]+/, "~")}</span>}{s.error && <span className="text-destructive">{s.error}</span>}</span>}>
            <span className="flex flex-wrap items-center justify-end gap-0.5 max-sm:justify-start">
              <LevelSel v={s.level} on={(l) => l && void patch(s, { level: l }, t("knowledge:sources.levelChanged"))} testId={"src-level-" + s.id} />
              <VisSel v={s.visibility} on={(x) => void patch(s, { visibility: x })} testId={"src-vis-" + s.id} />
              <Switch checked={!s.paused} onCheckedChange={(on) => void patch(s, { paused: !on }, on ? t("knowledge:sources.resumed") : t("knowledge:sources.paused"))} aria-label={s.paused ? t("knowledge:sources.resumeAria", { name: s.name }) : t("knowledge:sources.pauseAria", { name: s.name })} className="ml-1" data-source-on />
              {s.kind === "folder" && <DropdownMenu>
                <DropdownMenuTrigger render={<Button variant="ghost" size="icon-xs" aria-label={t("knowledge:sources.moreAria", { name: s.name })} className="text-muted-foreground" data-source-menu />}><MoreHorizontalIcon /></DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="min-w-36">
                  <DropdownMenuItem disabled={s.status === "syncing" || s.paused} onClick={() => void sync(s)} data-source-sync><RefreshCwIcon />{t("knowledge:sources.syncNow")}</DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem variant="destructive" onClick={() => void remove(s)} data-source-del><TrashIcon />{t("knowledge:sources.remove")}</DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>}
            </span>
          </Row>))}</Group>}
      <div className="-mt-2 px-1 type-footnote text-muted-foreground">{t("knowledge:sources.footer")}</div>
      <FolderDialog open={folder} onClose={() => setFolder(false)} onDone={() => void load()} />
    </div>
  )
}

/** "添加文件": the composer's upload path, then each uploaded file joins the knowledge base */
function AddFiles({ onDone }: { onDone: () => void }) {
  const t = useT()
  const up = useUploads(), input = useRef<HTMLInputElement>(null), added = useRef(new Set<string>())
  useEffect(() => {
    const fresh = up.ready.filter((f) => !added.current.has(f.id)); if (!fresh.length) return
    for (const f of fresh) added.current.add(f.id)
    Promise.allSettled(fresh.map((f) => kb.addFile(f.id))).then((rs) => {
      const bad = rs.find((r) => r.status === "rejected") as PromiseRejectedResult | undefined
      toast(bad ? errMsg(bad.reason) : t("knowledge:files.added", { names: fresh.map((f) => f.name).join(t("knowledge:listSep")) }))
      if (!up.uploading) up.clear()
      onDone()
    })
  }, [up.ready.length]) // eslint-disable-line react-hooks/exhaustive-deps
  const failed = up.list.find((p) => p.error)
  useEffect(() => { if (failed) { toast(t("knowledge:files.failed", { name: failed.name, error: failed.error })); up.remove(failed.key) } }, [failed?.key]) // eslint-disable-line react-hooks/exhaustive-deps
  return <>
    <input ref={input} type="file" multiple hidden accept=".md,.txt,.html,.pdf,.docx,text/*,application/pdf" onChange={(e) => { if (e.target.files) up.add(e.target.files); e.target.value = "" }} data-add-file-input />
    <Button size="sm" variant="outline" disabled={up.uploading} onClick={() => input.current?.click()} data-add-file>{up.uploading ? <Spinner className="size-3.5" /> : <UploadIcon />}{t("knowledge:files.add")}</Button>
  </>
}

const lines = (s: string) => s.split(/[\n,]/).map((x) => x.trim()).filter(Boolean)
function FolderDialog({ open, onClose, onDone }: { open: boolean; onClose: () => void; onDone: () => void }) {
  const t = useT()
  const [name, setName] = useState(""), [path, setPath] = useState(""), [inc, setInc] = useState("**/*.md\n**/*.txt"), [exc, setExc] = useState(".obsidian/**\nnode_modules/**"), [busy, setBusy] = useState(false), [err, setErr] = useState("")
  useEffect(() => { if (open) { setErr(""); setBusy(false) } }, [open])
  const save = async () => {
    if (!path.trim() || busy) return
    setBusy(true); setErr("")
    const p = path.trim(), n = name.trim() || p.replace(/\/+$/, "").split("/").pop() || t("knowledge:folder.defaultName")
    try { await kb.addFolder({ name: n.slice(0, 40), path: p, include: lines(inc), exclude: lines(exc) }); toast(t("knowledge:folder.added")); setName(""); setPath(""); onClose(); onDone() }
    catch (e) { setErr(errMsg(e)) } finally { setBusy(false) }
  }
  const field = (id: string, label: ReactNode, el: ReactNode, desc?: string) => <Field><FieldLabel htmlFor={id}>{label}</FieldLabel>{el}{desc && <FieldDescription>{desc}</FieldDescription>}</Field>
  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[520px]">
        <DialogHeader><DialogTitle className="type-headline">{t("knowledge:folder.title")}</DialogTitle><DialogDescription className="type-callout">{t("knowledge:folder.desc")}</DialogDescription></DialogHeader>
        <FieldGroup className="gap-4">
          {field("kb-folder-path", t("knowledge:folder.path"), <Input id="kb-folder-path" autoFocus className="font-mono" value={path} onChange={(e) => setPath(e.target.value)} placeholder="/Users/user/Documents/wiki" data-folder-path />, t("knowledge:folder.pathDesc"))}
          {field("kb-folder-name", t("knowledge:folder.name"), <Input id="kb-folder-name" maxLength={40} value={name} onChange={(e) => setName(e.target.value)} placeholder={t("knowledge:folder.namePlaceholder")} data-folder-name />)}
          <div className="grid gap-4 sm:grid-cols-2">
            {field("kb-folder-inc", t("knowledge:folder.include"), <Textarea id="kb-folder-inc" className="min-h-20 font-mono text-[12.5px]" value={inc} onChange={(e) => setInc(e.target.value)} data-folder-include />, t("knowledge:folder.includeDesc"))}
            {field("kb-folder-exc", t("knowledge:folder.exclude"), <Textarea id="kb-folder-exc" className="min-h-20 font-mono text-[12.5px]" value={exc} onChange={(e) => setExc(e.target.value)} data-folder-exclude />)}
          </div>
          {err && <div role="alert" className="type-callout text-destructive" data-folder-error>{err}</div>}
        </FieldGroup>
        <DialogFooter className="gap-2"><Button variant="ghost" onClick={onClose}>{t("common:action.cancel")}</Button><Button disabled={!path.trim() || busy} onClick={() => void save()} data-folder-save>{busy && <Spinner className="size-3.5" />}{t("common:action.add")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/* ───────── 待确认 ───────── */
function CandidatesTab({ onChange }: { onChange: (n: number) => void }) {
  const t = useT()
  const [xs, setXs] = useState<Candidate[] | null>(null), [err, setErr] = useState(""), [busy, setBusy] = useState(false)
  const load = useCallback(() => kb.candidates().then((a) => { setXs(a); onChange(a.length) }, (e) => setErr(errMsg(e))), [onChange])
  useEffect(() => { void load() }, [load])
  useHubEvents(["mm"], () => void load())
  const resolve = async (ids: string[], choice: "yes" | "no") => {
    if (!ids.length || busy) return
    setBusy(true)
    try { const r = await kb.resolve(ids, choice); toast(choice === "yes" ? t("knowledge:cand.confirmed", { count: r.done }) : t("knowledge:cand.ignored", { count: r.done })); await load() } catch (e) { toast(errMsg(e)) } finally { setBusy(false) }
  }
  if (err) return <Empty title={t("knowledge:cand.failed")} sub={err} />
  if (!xs) return <Loading />
  if (!xs.length) return <><Empty title={t("knowledge:cand.noneTitle")} sub={t("knowledge:cand.noneSub")} /><ExpiredCandidates onRestored={load} /></>
  /* the batch call takes at most 50 ids */
  const ids = xs.slice(0, 50).map((c) => c.mem.id)
  return (
    <section>
      <SectionLabel right={<span className="flex gap-1"><Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void resolve(ids, "no")} data-cand-all-no>{t("knowledge:cand.ignoreAll")}</Button><Button size="xs" variant="outline" disabled={busy} onClick={() => void resolve(ids, "yes")} data-cand-all-yes>{t("knowledge:cand.confirmAll")}</Button></span>}>
        {t("knowledge:cand.title", { count: xs.length })}</SectionLabel>
      <Group>{xs.map((c) => (
        <Row key={c.mem.id} data-candidate={c.mem.id} className={STACK}
          title={<span className="inline-flex flex-wrap items-center gap-2">{c.mem.text}<Badge variant="outline" className="font-normal text-muted-foreground" data-cand-record>{c.record ?? recordOf(c.mem.kind)}</Badge></span>}
          sub={<span className="flex flex-col gap-0.5"><span>{t("knowledge:cand.source", { kind: memKind(c.mem.kind), source: c.origin_label || c.mem.source_label })}</span>{c.why && <span className="break-words text-foreground/80" data-cand-why>{t("knowledge:cand.why", { why: c.why })}</span>}{c.scope && <span className="break-words">{t("knowledge:cand.scope", { scope: c.scope })}</span>}{c.evidence && <span className="break-words">{t("knowledge:cand.evidence", { evidence: c.evidence })}</span>}{c.supersedes && <span className="break-words">{t("knowledge:cand.supersedes", { text: c.supersedes.text })}</span>}</span>}>
          <span className="flex items-center gap-1">
            <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void resolve([c.mem.id], "no")} data-cand-no>{t("knowledge:cand.no")}</Button>
            <Button size="xs" variant="outline" disabled={busy} onClick={() => void resolve([c.mem.id], "yes")} data-cand-yes>{t("common:action.save")}</Button>
          </span>
        </Row>))}</Group>
      <div className="mt-2 px-1 type-footnote text-muted-foreground">{t("knowledge:cand.footer")}</div>
      <ExpiredCandidates onRestored={load} />
    </section>
  )
}

/** candidates that expired unanswered, folded away; one click puts one back */
function ExpiredCandidates({ onRestored }: { onRestored: () => void }) {
  const t = useT()
  const [xs, setXs] = useState<ExpiredCandidate[]>([]), [open, setOpen] = useState(false), [busy, setBusy] = useState(false)
  const load = useCallback(() => kb.expired().then(setXs, () => setXs([])), [])
  useEffect(() => { void load() }, [load])
  useHubEvents(["mm"], () => void load())
  if (!xs.length) return null
  const restore = async (id: string) => {
    if (busy) return
    setBusy(true)
    try { await kb.restore(id); toast(t("knowledge:cand.restored")); await load(); onRestored() } catch (e) { toast(errMsg(e)) } finally { setBusy(false) }
  }
  return (
    <section className="mt-6" data-cand-expired>
      <button type="button" className="px-1 type-footnote text-muted-foreground hover:underline" onClick={() => setOpen((o) => !o)} data-cand-expired-toggle>{t("knowledge:cand.expired", { count: xs.length })}</button>
      {open && <Group className="mt-2">{xs.map((c) => (
        <Row key={c.mem.id} data-cand-expired-row={c.mem.id} className={STACK} title={c.mem.text}
          sub={<span className="flex flex-col gap-0.5"><span>{t("knowledge:cand.source", { kind: memKind(c.mem.kind), source: c.origin_label || c.mem.source_label })}</span>{c.evidence && <span className="break-words">{t("knowledge:cand.evidence", { evidence: c.evidence })}</span>}</span>}>
          <Button size="xs" variant="outline" disabled={busy} onClick={() => void restore(c.mem.id)} data-cand-restore>{t("knowledge:cand.restore")}</Button>
        </Row>))}</Group>}
    </section>
  )
}

