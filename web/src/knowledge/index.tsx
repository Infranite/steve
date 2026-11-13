/* 知识 (knowledge.md §9): search, long-term memories, knowledge entries, notes and documents, sources, and
 * candidates waiting for you. The four kinds of record (ST-41): 事项记录 and 会话记录 are documents kept as
 * they are; 长期记忆 (preference, rule) and 知识条目 (fact, event) are what Steve distils from them, one tab each.
 * Every list is the Hub's; on a Hub without /api/kb the whole page is the "还没有接入" placeholder. The two memory
 * tabs are an affectUI Memory Panel (components/affect/memory-panel). */
import { useHubEvents } from "@/store"
import { Fragment, useCallback, useEffect, useRef, useState, type ReactNode } from "react"
import { cn } from "cn"
import { FilePlusIcon, FolderPlusIcon, MoreHorizontalIcon, PlusIcon, RefreshCwIcon, SearchIcon, TrashIcon, UploadIcon, XIcon } from "lucide-react"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Switch } from "@/components/ui/switch"
import { Spinner } from "@/components/ui/spinner"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Page, SectionLabel, Group, Row, Segmented, Chips, Empty, StatusDot, useScrollMore, go, DiscChevron, DISC_TRIGGER } from "@/ui"
import { toast, tm } from "@/store"
import { useUploads } from "@/attach"
import { kb, levelLabel, visLabel, memKind, recordOf, LONG_TERM, SOURCE_KIND, LEVEL_DOT, type Candidate, type ExpiredCandidate, type DocBrief, type Hit, type KbSource, type Mem } from "@/api/knowledge"
import { KbGate, LevelSel, META_SELECT, VisSel, errMsg } from "@/knowledge/fields"
import { DocView, NoteView } from "@/knowledge/doc"
import { MemoryPanel, type Memory, type PanelText } from "@/components/affect/memory-panel"
import { monthDayShort } from "@/i18n/format"
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

const TAB_FADE: Record<string, string> = {
  end: "[mask-image:linear-gradient(to_right,#000_80%,transparent)]",
  start: "[mask-image:linear-gradient(to_left,#000_80%,transparent)]",
  "start end": "[mask-image:linear-gradient(to_right,transparent,#000_15%,#000_80%,transparent)]",
}
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
  /* six tabs run past a phone's width: the cut edge fades, and the open tab scrolls into view */
  const [tabsRef, tabsMore] = useScrollMore<HTMLDivElement>("x")
  useEffect(() => { tabsRef.current?.querySelector<HTMLElement>(`[data-seg="${CSS.escape(tab)}"]`)?.scrollIntoView({ block: "nearest", inline: "nearest" }) }, [tab])
  const opts = [["搜索", t("knowledge:tab.search")], ["长期记忆", t("knowledge:tab.mems")], ["知识条目", t("knowledge:tab.entries")], ["笔记与文档", t("knowledge:tab.docs")], ["来源", t("knowledge:tab.sources")],
    ["待确认", <span key="p" className="inline-flex items-center gap-1">{t("knowledge:tab.pending")}{pend > 0 && <span className="rounded-full bg-attention/14 px-1.5 type-label tabular-nums text-attention" data-pending-count>{pend}</span>}</span>, t("knowledge:tab.pending")]] as const
  return (
    <Page title={t("knowledge:page.title")} sub={t("knowledge:page.sub")} max={760}>
      <div className="flex flex-col gap-5">
        <SearchBox onQuery={(q) => { query = q; if (q && tab !== "搜索") tab = "搜索"; r() }} />
        <div ref={tabsRef} className={cn("-mx-1 overflow-x-auto px-1 pb-0.5 [scrollbar-width:none]", TAB_FADE[tabsMore ?? ""])} data-more={tabsMore}><Segmented value={tab} onChange={setTab} options={opts} /></div>
        <div key={tab} className="anim-in" data-kb-tab={tab}>
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
    <div role="search" className="flex items-center gap-2.5 px-3.5 rounded-xl bg-card shadow-[var(--lift)] transition-[box-shadow] duration-180 focus-within:shadow-[var(--lift-hover)] focus-within:ring-[3px] focus-within:ring-ring/25">
      <SearchIcon className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.75} />
      <input type="search" aria-label={t("knowledge:search.aria")} value={v} onChange={(e) => set(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter") { clearTimeout(timer.current); onQuery(v.trim()) } }}
        placeholder={t("knowledge:search.placeholder")} maxLength={200} className="h-11 min-w-0 flex-1 bg-transparent type-body outline-none placeholder:text-tertiary [&::-webkit-search-cancel-button]:hidden" data-kb-search />
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
  return <>{text.split(re).map((p, n) => (n % 2 ? <mark key={n} className="rounded-md bg-attention/20 px-0.5 text-foreground">{p}</mark> : <Fragment key={n}>{p}</Fragment>))}</>
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
  const down = res.down && <p className="pb-2 type-footnote text-muted-foreground" role="note" data-kb-keywords-only>{t("knowledge:search.keywordsOnly")}</p>
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
const aboutName = (a: Mem["about"][number]) => a.name ?? (a.type === "party" ? tt("knowledge:mems.aboutParty", { id: a.id }) : a.type === "item" ? tt("knowledge:mems.aboutItem", { id: a.id }) : a.id)
/** the kinds each record keeps, in the order the panel groups them; the first is what a new one becomes */
const KINDS: Record<RecordName, string[]> = { 长期记忆: ["rule", "preference"], 知识条目: ["fact", "event", "summary"] }
/** how long a forgotten memory can still come back before the Hub deletes it */
const UNDO_MS = 6000

/** every word the panel shows in Steve (no memory switch, no search of its own: the page has one) */
type Unshown = "memoryOff" | "off" | "useMemory" | "offNote" | "search" | "closeSearch" | "noneFor"
/** the rest are per record: none, newMemory, newPlaceholder */
const panelText = (): Omit<PanelText, Unshown | "none" | "newMemory" | "newPlaceholder"> => ({
  paused: tt("knowledge:mems.stale"), cancel: tt("common:action.cancel"), save: tt("common:action.save"), edit: tt("knowledge:panel.edit"),
  pause: tt("knowledge:panel.pause"), resume: tt("knowledge:panel.resume"), forget: tt("knowledge:panel.forget"), undo: tt("knowledge:panel.undo"),
  editAria: tt("knowledge:panel.editAria"), editOf: (text) => tt("knowledge:panel.editOf", { text }), pauseOf: (text) => tt("knowledge:panel.pauseOf", { text }),
  resumeOf: (text) => tt("knowledge:panel.resumeOf", { text }), forgetOf: (text) => tt("knowledge:mems.deleteAria", { text }),
  memories: (count) => tt("knowledge:panel.memories", { count }), pausedN: (count) => tt("knowledge:panel.pausedN", { count }),
  forgot: [tt("knowledge:panel.forgotBefore"), tt("knowledge:panel.forgotAfter")], editing: tt("knowledge:panel.editing"), adding: tt("knowledge:panel.adding"),
  other: tt("knowledge:panel.other"), used: tt("knowledge:panel.used"), notUsed: tt("knowledge:panel.notUsed"), category: tt("knowledge:panel.category"),
  all: tt("knowledge:all"),
  noneHere: tt("knowledge:panel.noneHere"), newCategory: tt("knowledge:panel.newCategory"), general: tt("knowledge:mems.general"),
})

/** 长期记忆 and 知识条目: an affectUI Memory Panel over the Hub's memories — grouped by kind, edited in place, marked
 *  stale (kept, no longer used) or forgotten with a few seconds to undo before the Hub deletes it; each keeps its
 *  disclosure level and visibility pickers, and candidates say they still wait for you in 待确认. */
function MemsTab({ record, focus, onRecord }: { record: RecordName; focus: string | null; onRecord: (r: RecordName) => void }) {
  const t = useT()
  const R = RECORD[record], k = R.key
  const [xs, setXs] = useState<Mem[] | null>(null), [err, setErr] = useState("")
  /* forgotten ones: deleted on the Hub once Undo has passed (or when the page goes); until then a reload leaves them out */
  const gone = useRef(new Map<string, { m: Mem; at: number; timer: number }>())
  /* deleted early because the app was hidden while Undo still showed: Undo writes them back */
  const left = useRef(new Map<string, Mem>())
  const load = useCallback(() => kb.mems().then((a) => setXs(a.filter((m) => !gone.current.has(m.id))), (e) => setErr(errMsg(e))), [])
  useEffect(() => { void load() }, [load])
  useHubEvents(["mm"], () => void load())
  const drop = useCallback((id: string) => {
    const g = gone.current.get(id); if (!g) return
    clearTimeout(g.timer); gone.current.delete(id)
    kb.deleteMem(id).catch((e) => { toast(errMsg(e)); void load() })
  }, [load])
  useEffect(() => () => { for (const id of [...gone.current.keys()]) drop(id) }, [drop])
  /* the page going away (reload, tab closed, app hidden) fires neither the timer nor unmount: send the deletes now */
  useEffect(() => {
    const flush = () => { for (const [id, g] of gone.current) { clearTimeout(g.timer); kb.deleteMemOnLeave(id); left.current.set(id, g.m) } gone.current.clear() }
    const hidden = () => { if (document.visibilityState === "hidden") flush() }
    addEventListener("pagehide", flush); document.addEventListener("visibilitychange", hidden)
    return () => { removeEventListener("pagehide", flush); document.removeEventListener("visibilitychange", hidden) }
  }, [])
  /* a linked memory opens on the tab it belongs to */
  const focused = focus ? xs?.find((m) => m.id === focus) : undefined
  useEffect(() => { if (focused && recordTab(focused.kind) !== record) onRecord(recordTab(focused.kind)) }, [focused, record, onRecord])
  useEffect(() => { if (focus && xs) document.querySelector(`[data-mem="${CSS.escape(focus)}"]`)?.scrollIntoView({ block: "center" }) }, [focus, xs])
  const kinds = KINDS[record], kindOf = (label: string) => kinds.find((c) => memKind(c) === label) ?? R.kind
  const patch = async (id: string, p: Parameters<typeof kb.patchMem>[1]) => {
    setXs((a) => a?.map((x) => (x.id === id ? { ...x, ...p } : x)) ?? null)
    try { const n = await kb.patchMem(id, p); setXs((a) => a?.map((x) => (x.id === id ? { ...x, ...n } : x)) ?? null) } catch (e) { toast(errMsg(e)); void load() }
  }
  const add = async (text: string, label: string) => {
    try { await kb.addMem({ text, kind: kindOf(label) }); toast(t("knowledge:mems.added")); await load() } catch (e) { toast(errMsg(e)) }
  }
  const forget = (id: string) => {
    const at = xs?.findIndex((x) => x.id === id) ?? -1; if (!xs || at < 0) return
    setXs((a) => a?.filter((x) => x.id !== id) ?? null)
    gone.current.set(id, { m: xs[at], at, timer: window.setTimeout(() => drop(id), UNDO_MS) })
  }
  const restore = (id: string) => {
    const l = left.current.get(id)
    if (l) { left.current.delete(id); kb.addMem({ text: l.text, kind: l.kind, level: l.level, visibility: l.visibility }).then(() => load(), (e) => toast(errMsg(e))); return }
    const g = gone.current.get(id); if (!g) return
    clearTimeout(g.timer); gone.current.delete(id)
    setXs((a) => (a ? [...a.slice(0, g.at), g.m, ...a.slice(g.at)] : a))
  }
  const mine = (xs ?? []).filter((m) => recordTab(m.kind) === record)
  const byId = new Map(mine.map((m) => [m.id, m]))
  const memories: Memory[] = mine.map((m) => ({
    id: m.id, text: m.text, category: memKind(m.kind), paused: m.status === "stale",
    source: [m.about.length ? tt("knowledge:mems.aboutLabel", { names: m.about.map(aboutName).join(tt("knowledge:listSep")) }) : "", t("knowledge:panel.source", { source: m.source_label || t("knowledge:mems.youWrote") })].filter(Boolean).join(" · "),
    /* labelled: a source like "11-12 的对话" carries its own date */
    date: m.created ? t("knowledge:panel.saved", { date: monthDayShort(m.created) }) : undefined,
    tag: m.status === "candidate" ? t("knowledge:mems.pending") : undefined, canPause: m.status !== "candidate",
  }))
  return (
    <div className="flex flex-col gap-4">
      {err ? <Empty title={t(`knowledge:record.${k}.failed`)} sub={err} /> : !xs ? <Loading /> : !mine.length && !gone.current.size ? <>
        <Empty title={t(`knowledge:record.${k}.none`)} sub={t(`knowledge:record.${k}.empty`)} />
        <AddFirst record={record} onAdd={(text) => add(text, memKind(R.kind))} />
      </> :
        <MemoryPanel className="[--ui-mp-width:100%]" title={t(`knowledge:tab.${k === "mem" ? "mems" : "entries"}`)} showTitle={false} searchable={false} memories={memories} categories={kinds.map(memKind)}
          maxHeight={0} undoFor={UNDO_MS} showSwitch={false} focus={focus} text={{ ...panelText(), none: t(`knowledge:record.${k}.none`), newMemory: t(`knowledge:record.${k}.addAria`), newPlaceholder: t(`knowledge:record.${k}.hint`) } satisfies Omit<PanelText, Unshown>}
          addedSource={t("knowledge:mems.youWrote")} onChange={() => {}}
          onAdd={(m) => void add(m.text, m.category)} onEdit={(id, text) => void patch(id, { text })}
          onPause={(id, paused) => void patch(id, { status: paused ? "stale" : "confirmed" })} onForget={forget} onRestore={(m) => restore(m.id)}
          renderMeta={(x) => { const m = byId.get(x.id); return m && <>
            <LevelSel v={m.level} on={(l) => l && void patch(m.id, { level: l })} testId={"mem-level-" + m.id} className={META_SELECT} />
            <VisSel v={m.visibility} on={(v) => void patch(m.id, { visibility: v })} testId={"mem-vis-" + m.id} className={META_SELECT} />
          </> }} />}
      <div className="type-footnote text-muted-foreground">{t("knowledge:mems.footer", { note: t(`knowledge:record.${k}.note`) })}</div>
    </div>
  )
}
/** an empty record: the one line to write its first entry */
function AddFirst({ record, onAdd }: { record: RecordName; onAdd: (text: string) => Promise<void> }) {
  const t = useT(), k = RECORD[record].key
  const [v, setV] = useState(""), [busy, setBusy] = useState(false)
  const go = async () => { const text = v.trim(); if (!text || busy) return; setBusy(true); try { await onAdd(text); setV("") } finally { setBusy(false) } }
  return (
    <div className="flex items-center gap-2 rounded-xl bg-card py-1.5 pl-3.5 pr-1.5 shadow-[var(--lift)] transition-[box-shadow] duration-180 focus-within:shadow-[var(--lift-hover)] focus-within:ring-[3px] focus-within:ring-ring/25">
      <input aria-label={t(`knowledge:record.${k}.addAria`)} value={v} onChange={(e) => setV(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && !e.nativeEvent.isComposing) void go() }} maxLength={2000}
        placeholder={t(`knowledge:record.${k}.hint`)} className="h-8 min-w-0 flex-1 bg-transparent type-body outline-none placeholder:text-tertiary" data-mem-input />
      <Button size="sm" disabled={!v.trim() || busy} onClick={() => void go()} data-mem-add>{busy ? <Spinner className="size-3.5" /> : <PlusIcon />}{t("knowledge:mems.add")}</Button>
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
          <Field><FieldLabel htmlFor="kb-note-body">{t("knowledge:newNote.bodyLabel")}</FieldLabel><Textarea id="kb-note-body" className="min-h-40 font-mono" value={body} onChange={(e) => setBody(e.target.value)} placeholder={t("knowledge:newNote.bodyPlaceholder")} data-note-body
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
            title={<span className="inline-flex flex-wrap items-center gap-2">{s.name}<span className="type-footnote text-muted-foreground">{SOURCE_KIND[s.kind] ?? s.kind}</span>{s.status === "error" && <Badge variant="destructive">{t("knowledge:sources.error")}</Badge>}</span>}
            sub={<span className="flex flex-col"><span data-source-stats>{statsLine(s)}</span>{s.path && <span className="truncate font-mono">{s.path.replace(/^\/Users\/[^/]+/, "~")}</span>}{s.error && <span className="text-destructive">{s.error}</span>}</span>}>
            <span className="flex flex-wrap items-center justify-end gap-0.5 max-sm:justify-start">
              <LevelSel v={s.level} on={(l) => l && void patch(s, { level: l }, t("knowledge:sources.levelChanged"))} testId={"src-level-" + s.id} className={cn(META_SELECT, "h-7! px-2 hover:bg-accent")} />
              <VisSel v={s.visibility} on={(x) => void patch(s, { visibility: x })} testId={"src-vis-" + s.id} className={cn(META_SELECT, "h-7! px-2 hover:bg-accent")} />
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
      <div className="-mt-3 type-footnote text-muted-foreground">{t("knowledge:sources.footer")}</div>
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
            {field("kb-folder-inc", t("knowledge:folder.include"), <Textarea id="kb-folder-inc" className="min-h-20 font-mono" value={inc} onChange={(e) => setInc(e.target.value)} data-folder-include />, t("knowledge:folder.includeDesc"))}
            {field("kb-folder-exc", t("knowledge:folder.exclude"), <Textarea id="kb-folder-exc" className="min-h-20 font-mono" value={exc} onChange={(e) => setExc(e.target.value)} data-folder-exclude />)}
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
          title={<span className="inline-flex flex-wrap items-center gap-2">{c.mem.text}<Badge variant="secondary" className="text-muted-foreground" data-cand-record>{c.record ?? recordOf(c.mem.kind)}</Badge></span>}
          sub={<span className="flex flex-col gap-0.5"><span>{t("knowledge:cand.source", { kind: memKind(c.mem.kind), source: c.origin_label || c.mem.source_label })}</span>{c.why && <span className="break-words text-foreground/80" data-cand-why>{t("knowledge:cand.why", { why: c.why })}</span>}{c.scope && <span className="break-words">{t("knowledge:cand.scope", { scope: c.scope })}</span>}{c.evidence && <span className="break-words">{t("knowledge:cand.evidence", { evidence: c.evidence })}</span>}{c.supersedes && <span className="break-words">{t("knowledge:cand.supersedes", { text: c.supersedes.text })}</span>}</span>}>
          <span className="flex items-center gap-1">
            <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void resolve([c.mem.id], "no")} data-cand-no>{t("knowledge:cand.no")}</Button>
            <Button size="xs" variant="outline" disabled={busy} onClick={() => void resolve([c.mem.id], "yes")} data-cand-yes>{t("common:action.save")}</Button>
          </span>
        </Row>))}</Group>
      <div className="mt-3 type-footnote text-muted-foreground">{t("knowledge:cand.footer")}</div>
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
    <Collapsible open={open} onOpenChange={setOpen} render={<section />} className="mt-6" data-cand-expired>
      <CollapsibleTrigger className={DISC_TRIGGER + " type-footnote"} data-cand-expired-toggle><DiscChevron className="size-3" />{t("knowledge:cand.expired", { count: xs.length })}</CollapsibleTrigger>
      <CollapsibleContent><Group className="pt-2">{xs.map((c) => (
        <Row key={c.mem.id} data-cand-expired-row={c.mem.id} className={STACK} title={c.mem.text}
          sub={<span className="flex flex-col gap-0.5"><span>{t("knowledge:cand.source", { kind: memKind(c.mem.kind), source: c.origin_label || c.mem.source_label })}</span>{c.evidence && <span className="break-words">{t("knowledge:cand.evidence", { evidence: c.evidence })}</span>}</span>}>
          <Button size="xs" variant="outline" disabled={busy} onClick={() => void restore(c.mem.id)} data-cand-restore>{t("knowledge:cand.restore")}</Button>
        </Row>))}</Group></CollapsibleContent>
    </Collapsible>
  )
}

