/* 知识: organized by the owner's world — 人 · 组织 · 项目 · 偏好与规则 · 资料, plus 待确认 (knowledge candidates
 * only) and 整理 (deterministic knowledge steward). Search keeps knowledge hits and work-item/chat records strictly
 * separated. Preferences & rules use affectUI's Memory Panel (components/affect/memory-panel). */
import { useHubEvents } from "@/store"
import { Fragment, useCallback, useEffect, useRef, useState, type ReactNode } from "react"
import { cn } from "cn"
import { BotIcon, FilePlusIcon, FileTextIcon, FolderPlusIcon, MailIcon, MessagesSquareIcon, MoreHorizontalIcon, PlusIcon, PuzzleIcon, RefreshCwIcon, SearchIcon, SparklesIcon, TrashIcon, UploadIcon, UserIcon, UsersIcon, ZapIcon } from "lucide-react"
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
import { Page, SectionLabel, Group, Panel, Row, Segmented, Chips, Empty, Blank, Meta, Tile, StatusDot, useScrollMore, go, DiscChevron, DISC_TRIGGER, SearchField } from "@/ui"
import { toast, tm } from "@/store"
import { useUploads } from "@/attach"
import { kb, levelLabel, visLabel, memKind, LONG_TERM, SOURCE_KIND, LEVEL_DOT, type Candidate, type ExpiredCandidate, type DocBrief, type Hit, type KbSource, type Mem, type MemProvider, type RecordHit, type SearchScope } from "@/api/knowledge"
import { LevelSel, META_SELECT, VisSel, errMsg } from "@/knowledge/fields"
import { DocView, NoteView } from "@/knowledge/doc"
import { SubjectsTab, SubjectView } from "@/knowledge/subjects"
import { StewardTab } from "@/knowledge/steward"
import { OriginLine, originText, parseSubject, savedLine, subjectHref } from "@/knowledge/origin"
import { MemoryPanel, type Memory, type PanelText } from "@/components/affect/memory-panel"
import { t as tt, useT } from "@/i18n"

/* tab values are stable ASCII ids (data-seg / data-kb-tab); labels come from knowledge:tab.* */
type Tab = "people" | "orgs" | "projects" | "rules" | "materials" | "pending" | "steward"
let tab: Tab = "rules", query = "", tabHash = ""

/** `#/knowledge`, `#/knowledge?mem=…`, `#/knowledge?tab=people|orgs|projects|rules|materials|pending|steward`,
 *  `#/knowledge/doc/:id?seq=`, `#/knowledge/note/:id`, `#/knowledge/about/<type>:<id>` */
export function KnowledgeView({ sub, id }: { sub?: string; id?: string }) {
  const [docId] = (id ?? "").split("?")
  const subject = sub === "about" ? parseSubject(docId) : null
  return sub === "doc" && docId ? <DocView id={docId} /> : sub === "note" && docId ? <NoteView id={docId} /> : subject ? <SubjectView key={docId} type={subject.type} id={subject.id} /> : <KnowledgeHome />
}

const hashParam = (k: string) => new URLSearchParams(location.hash.split("?")[1] ?? "").get(k)

const LINKED_TAB: Record<string, Tab> = {
  people: "people",
  orgs: "orgs",
  projects: "projects",
  rules: "rules",
  materials: "materials",
  pending: "pending",
  steward: "steward",
  subjects: "people",
}

const TAB_FADE: Record<string, string> = {
  end: "[mask-image:linear-gradient(to_right,#000_80%,transparent)]",
  start: "[mask-image:linear-gradient(to_left,#000_80%,transparent)]",
  "start end": "[mask-image:linear-gradient(to_right,transparent,#000_15%,#000_80%,transparent)]",
}

function KnowledgeHome() {
  const t = useT()
  const [, f] = useState(0), r = () => f((x) => x + 1)
  const focusMem = hashParam("mem")
  if (focusMem && tab !== "rules") tab = "rules"
  const linked = LINKED_TAB[hashParam("tab") ?? ""]
  if (linked && tabHash !== location.hash) { tab = linked; query = ""; tabHash = location.hash }
  else if (!linked) tabHash = ""
  const urlQ = hashParam("q")
  if (urlQ && tabHash !== location.hash) { query = urlQ; tabHash = location.hash }
  const [pend, setPend] = useState(0)
  const loadPend = useCallback(() => { kb.candidates().then((xs) => setPend(xs.length), () => {}) }, [])
  useEffect(loadPend, [loadPend])
  useHubEvents(["mm", "kb"], loadPend)
  const setTab = (x: Tab) => { tab = x; query = ""; r() }
  const [tabsRef, tabsMore] = useScrollMore<HTMLDivElement>("x")
  useEffect(() => { tabsRef.current?.querySelector<HTMLElement>(`[data-seg="${CSS.escape(tab)}"]`)?.scrollIntoView({ block: "nearest", inline: "nearest" }) }, [tab])
  const opts = [
    ["people", t("knowledge:tab.people")],
    ["orgs", t("knowledge:tab.orgs")],
    ["projects", t("knowledge:tab.projects")],
    ["rules", t("knowledge:tab.rules")],
    ["materials", t("knowledge:tab.materials")],
    ["pending", <span key="p" className="inline-flex items-center gap-1">{t("knowledge:tab.pending")}{pend > 0 && <span className="rounded-full bg-attention/14 px-1.5 type-label tabular-nums text-attention" data-pending-count>{pend}</span>}</span>, t("knowledge:tab.pending")],
    ["steward", t("knowledge:tab.steward")],
  ] as const
  const activeKey = query ? "search" : tab
  return (
    <Page title={t("knowledge:page.title")} sub={t("knowledge:page.sub")} max={760}>
      <div className="flex flex-col gap-5">
        <SearchBox value={query} onQuery={(q) => { query = q; r() }} />
        <div ref={tabsRef} className={cn("-mx-1 overflow-x-auto px-1 pb-0.5 [scrollbar-width:none]", TAB_FADE[tabsMore ?? ""])} data-more={tabsMore}><Segmented value={tab} onChange={setTab} options={opts} /></div>
        <div key={activeKey} className="anim-in" data-kb-tab={activeKey}>
          {query ? <SearchTab q={query} />
            : tab === "people" ? <SubjectsTab category="person" />
            : tab === "orgs" ? <SubjectsTab category="org" />
            : tab === "projects" ? <SubjectsTab category="project" />
            : tab === "rules" ? <RulesTab focus={focusMem} />
            : tab === "materials" ? <MaterialsTab />
            : tab === "pending" ? <CandidatesTab onChange={setPend} />
            : <StewardTab />}
        </div>
      </div>
    </Page>
  )
}

function SearchBox({ value, onQuery }: { value: string; onQuery: (q: string) => void }) {
  const t = useT()
  const [v, setV] = useState(value), timer = useRef<number>(0)
  useEffect(() => { setV(value) }, [value])
  const set = (x: string) => { setV(x); clearTimeout(timer.current); timer.current = window.setTimeout(() => onQuery(x.trim()), 250) }
  return (
    <SearchField size="lg" type="search" aria-label={t("knowledge:search.aria")} value={v} onChange={(e) => set(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter") { clearTimeout(timer.current); onQuery(v.trim()) } }}
      placeholder={t("knowledge:search.placeholder")} maxLength={200} clearable onClear={() => { clearTimeout(timer.current); setV(""); onQuery("") }} data-kb-search />
  )
}

/* ───────── 搜索（知识与记录分离） ───────── */
function Highlight({ text, q }: { text: string; q: string }) {
  const terms = q.split(/\s+/).filter(Boolean).map((x) => x.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"))
  if (!terms.length) return <>{text}</>
  const re = new RegExp(`(${terms.join("|")})`, "gi")
  return <>{text.split(re).map((p, n) => (n % 2 ? <mark key={n} className="rounded-md bg-attention/20 px-0.5 text-foreground">{p}</mark> : <Fragment key={n}>{p}</Fragment>))}</>
}
const LevelTag = ({ l }: { l: string }) => <span className="inline-flex shrink-0 items-center gap-1 type-footnote text-muted-foreground" data-level={l}><StatusDot s={LEVEL_DOT[l] ?? "idle"} />{levelLabel(l)}</span>

function SearchTab({ q }: { q: string }) {
  const t = useT()
  const [scope, setScope] = useState<SearchScope>("all")
  const [res, setRes] = useState<{ q: string; scope: SearchScope; hits: Hit[]; records: RecordHit[]; ms: number; down: boolean } | null>(null)
  const [err, setErr] = useState("")
  useEffect(() => {
    if (!q) { setRes(null); return }
    let on = true; setErr("")
    kb.search(q, 20, scope).then(
      (x) => on && setRes({ q, scope, hits: x.hits ?? [], records: x.records ?? [], ms: x.took_ms, down: !!x.semantic_down }),
      (e) => on && setErr(errMsg(e)),
    )
    return () => { on = false }
  }, [q, scope])
  const scopeOpts: { k: SearchScope; l: string }[] = [
    { k: "all", l: t("knowledge:search.scopeAll") },
    { k: "knowledge", l: t("knowledge:search.scopeKnowledge") },
    { k: "records", l: t("knowledge:search.scopeRecords") },
  ]
  const curLabel = scopeOpts.find((x) => x.k === scope)?.l ?? scopeOpts[0].l
  if (!q) return <Empty icon={<SearchIcon />} title={t("knowledge:search.idleTitle")} sub={t("knowledge:search.idleSub")} />
  if (err) return <Empty title={t("knowledge:search.failed")} sub={err} />
  if (!res || res.q !== q || res.scope !== scope) return <div className="flex justify-center py-12" role="status" aria-label={t("knowledge:search.searching")}><Spinner className="size-5 text-muted-foreground" /></div>
  const down = res.down && scope !== "records" && <p className="pb-2 type-footnote text-muted-foreground" role="note" data-kb-keywords-only>{t("knowledge:search.keywordsOnly")}</p>
  const total = res.hits.length + res.records.length
  return (
    <div className="flex flex-col gap-5" aria-label={t("knowledge:search.results")}>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <Chips value={curLabel} onChange={(label) => { const found = scopeOpts.find((x) => x.l === label); if (found) setScope(found.k) }} options={scopeOpts.map((x) => x.l)} />
        {total > 0 && <span className="type-footnote text-muted-foreground">{t("knowledge:search.count", { count: total, ms: res.ms })}</span>}
      </div>
      {down}
      {total === 0 ? <Empty title={t("knowledge:search.noneTitle")} sub={t("knowledge:search.noneSub", { q })} /> : <>
        {res.hits.length > 0 && <section data-search-knowledge>
          <SectionLabel>{t("knowledge:search.knowledgeSection", { count: res.hits.length })}</SectionLabel>
          <Group>{res.hits.map((h) => (
            <Row key={h.type + h.id + h.seq} onClick={() => go(h.href)} data-hit={h.id}
              title={<span className="flex min-w-0 items-baseline gap-2"><span className="min-w-0 truncate"><Highlight text={h.title} q={q} />{h.heading ? <span className="text-muted-foreground"> · <Highlight text={h.heading} q={q} /></span> : null}</span></span>}
              sub={<span className="flex flex-col gap-1"><span className="line-clamp-3 type-callout text-foreground/80"><Highlight text={h.text} q={q} /></span><span className="truncate">{h.label}</span></span>}>
              <LevelTag l={h.level} />
            </Row>))}</Group>
        </section>}
        {res.records.length > 0 && <section data-search-records>
          <SectionLabel>{t("knowledge:search.recordsSection", { count: res.records.length })}</SectionLabel>
          <Group>{res.records.map((rec) => (
            <Row key={rec.record_kind + rec.id} onClick={() => go(rec.href)} data-record={rec.id} data-record-kind={rec.record_kind}
              icon={<Tile I={rec.record_kind === "item" ? FileTextIcon : MessagesSquareIcon} />}
              title={<span className="truncate"><Highlight text={rec.title} q={q} /></span>}
              sub={<span className="flex flex-col gap-1"><span className="line-clamp-3 type-callout text-foreground/80"><Highlight text={rec.text} q={q} /></span><Meta inline><span>{t(rec.record_kind === "item" ? "knowledge:search.recordItem" : "knowledge:search.recordChat")}</span><span>{tm(rec.updated)}</span></Meta></span>} />
          ))}</Group>
        </section>}
      </>}
    </div>
  )
}

/* ───────── 偏好与规则 ───────── */
const RULE_KINDS = ["rule", "preference"]
const UNDO_MS = 6000

type Unshown = "memoryOff" | "off" | "useMemory" | "offNote" | "search" | "closeSearch" | "noneFor"
export const panelText = (): Omit<PanelText, Unshown | "none" | "newMemory" | "newPlaceholder"> => ({
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

/** 偏好与规则: affectUI Memory Panel over the owner's stable rules and preferences. */
function RulesTab({ focus }: { focus: string | null }) {
  const t = useT()
  const [xs, setXs] = useState<Mem[] | null>(null), [err, setErr] = useState("")
  const gone = useRef(new Map<string, { m: Mem; at: number; timer: number }>())
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
  useEffect(() => {
    const flush = () => { for (const [id, g] of gone.current) { clearTimeout(g.timer); kb.deleteMemOnLeave(id); left.current.set(id, g.m) } gone.current.clear() }
    const hidden = () => { if (document.visibilityState === "hidden") flush() }
    addEventListener("pagehide", flush); document.addEventListener("visibilitychange", hidden)
    return () => { removeEventListener("pagehide", flush); document.removeEventListener("visibilitychange", hidden) }
  }, [])
  /* If a linked `?mem=` is a subject-bound fact/event, jump to its subject page. */
  const focused = focus ? xs?.find((m) => m.id === focus) : undefined
  useEffect(() => {
    if (focused && !LONG_TERM.includes(focused.kind) && focused.about.length > 0) {
      go(subjectHref(focused.about[0]))
    }
  }, [focused])
  useEffect(() => { if (focus && xs) document.querySelector(`[data-mem="${CSS.escape(focus)}"]`)?.scrollIntoView({ block: "center" }) }, [focus, xs])
  const kindOf = (label: string) => RULE_KINDS.find((c) => memKind(c) === label) ?? "rule"
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
  const mine = (xs ?? []).filter((m) => LONG_TERM.includes(m.kind))
  const byId = new Map(mine.map((m) => [m.id, m]))
  const memories: Memory[] = mine.map((m) => ({
    id: m.id, text: m.text, category: memKind(m.kind), paused: m.status === "stale",
    source: originText(m), sourceNode: <span data-mem-origin><OriginLine m={m} /></span>,
    date: savedLine(m) || undefined,
    tag: m.status === "candidate" ? t("knowledge:mems.pending") : undefined, canPause: m.status !== "candidate",
  }))
  return (
    <div className="flex flex-col gap-4">
      {err ? <Empty title={t("knowledge:record.mem.failed")} sub={err} /> : !xs ? <Loading /> : !mine.length && !gone.current.size ? <>
        <Empty title={t("knowledge:record.mem.none")} sub={t("knowledge:record.mem.empty")} />
        <AddFirst onAdd={(text) => add(text, memKind("rule"))} />
      </> :
        <MemoryPanel className="[--ui-mpc-width:100%]" title={t("knowledge:tab.rules")} showTitle={false} searchable={false} memories={memories} categories={RULE_KINDS.map(memKind)}
          maxHeight={0} undoFor={UNDO_MS} showSwitch={false} focus={focus} text={{ ...panelText(), none: t("knowledge:record.mem.none"), newMemory: t("knowledge:record.mem.addAria"), newPlaceholder: t("knowledge:record.mem.hint") } satisfies Omit<PanelText, Unshown>}
          addedSource={t("knowledge:mems.youWrote")} onChange={() => {}}
          onAdd={(m) => void add(m.text, m.category)} onEdit={(id, text) => void patch(id, { text })}
          onPause={(id, paused) => void patch(id, { status: paused ? "stale" : "confirmed" })} onForget={forget} onRestore={(m) => restore(m.id)}
          renderMeta={(x) => { const m = byId.get(x.id); return m && <>
            <LevelSel v={m.level} on={(l) => l && void patch(m.id, { level: l })} testId={"mem-level-" + m.id} className={META_SELECT} />
            <VisSel v={m.visibility} on={(v) => void patch(m.id, { visibility: v })} testId={"mem-vis-" + m.id} className={META_SELECT} />
          </> }} />}
      <div className="type-footnote text-muted-foreground">{t("knowledge:mems.footer", { note: t("knowledge:record.mem.note") })}</div>
    </div>
  )
}

function AddFirst({ onAdd }: { onAdd: (text: string) => Promise<void> }) {
  const t = useT()
  const [v, setV] = useState(""), [busy, setBusy] = useState(false)
  const submit = async () => { const text = v.trim(); if (!text || busy) return; setBusy(true); try { await onAdd(text); setV("") } finally { setBusy(false) } }
  return (
    <div className="flex items-center gap-2 rounded-xl bg-card py-1.5 pl-3.5 pr-1.5 shadow-[var(--lift)] transition-[box-shadow] duration-180 focus-within:shadow-[var(--lift-hover)] focus-within:ring-[3px] focus-within:ring-ring/25">
      <input aria-label={t("knowledge:record.mem.addAria")} value={v} onChange={(e) => setV(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && !e.nativeEvent.isComposing) void submit() }} maxLength={2000}
        placeholder={t("knowledge:record.mem.hint")} className="h-8 min-w-0 flex-1 bg-transparent type-body outline-none placeholder:text-tertiary" data-mem-input />
      <Button size="sm" disabled={!v.trim() || busy} onClick={() => void submit()} data-mem-add>{busy ? <Spinner className="size-3.5" /> : <PlusIcon />}{t("knowledge:mems.add")}</Button>
    </div>
  )
}

const STACK = "max-sm:[&>[data-slot=item-actions]]:w-full max-sm:[&>[data-slot=item-actions]]:justify-start max-sm:[&>[data-slot=item-actions]]:-ml-2"
const Loading = () => { const t = useT(); return <div className="flex justify-center py-12" role="status" aria-label={t("knowledge:page.loading")}><Spinner className="size-5 text-muted-foreground" /></div> }

/* ───────── 资料（笔记、文件、文件夹与来源） ───────── */
export const docHref = (d: { id: string; source: { kind: string } }) => (d.source.kind === "notes" ? "#/knowledge/note/" : "#/knowledge/doc/") + d.id

const statsLine = (s: KbSource) => {
  const x = s.stats ?? {}, parts = [tt("knowledge:sources.docs", { count: x.docs ?? 0 }), tt("knowledge:sources.chunks", { count: x.chunks ?? 0 })]
  if (x.skipped) parts.push(tt("knowledge:sources.skipped", { count: x.skipped }))
  if (x.kept) parts.push(tt("knowledge:sources.kept", { count: x.kept })); else if (x.incomplete?.length) parts.push(tt("knowledge:sources.incomplete"))
  if (s.status === "syncing") parts.push(tt("knowledge:sources.syncing")); else if (s.last_sync) parts.push(tt("knowledge:sources.lastSync", { time: tm(s.last_sync) })); else if (!s.builtin) parts.push(tt("knowledge:sources.neverSynced")); else parts.push(tt("knowledge:sources.live"))
  return parts.join(" · ")
}
const sourceState = (s: KbSource): [string, string] => (s.paused ? ["idle", tt("knowledge:sources.state.paused")] : s.status === "error" ? ["fail", tt("knowledge:sources.state.error")] : s.status === "syncing" ? ["run", tt("knowledge:sources.state.syncing")] : ["done", tt("knowledge:sources.state.ok")])

function MaterialsTab() {
  const t = useT()
  const [docs, setDocs] = useState<DocBrief[] | null>(null), [docErr, setDocErr] = useState(""), [src, setSrc] = useState("")
  const [sources, setSources] = useState<KbSource[] | null>(null), [srcErr, setSrcErr] = useState("")
  const [prov, setProv] = useState<{ memory: MemProvider[]; store: string } | null>(null)
  const [newNote, setNewNote] = useState(false), [folder, setFolder] = useState(false)
  const loadDocs = useCallback(() => kb.docs().then(setDocs, (e) => setDocErr(errMsg(e))), [])
  const loadSources = useCallback(() => kb.sources().then(setSources, (e) => setSrcErr(errMsg(e))), [])
  const loadProv = useCallback(() => kb.providers().then(setProv, () => setProv(null)), [])
  useEffect(() => { void loadDocs(); void loadSources(); void loadProv() }, [loadDocs, loadSources, loadProv])
  useHubEvents(["kb"], () => { void loadDocs(); void loadSources() })
  useHubEvents(["mm"], () => void loadProv())
  const syncing = sources?.some((s) => s.status === "syncing")
  useEffect(() => { if (!syncing) return; const iv = setInterval(() => { void loadSources(); void loadDocs() }, 2000); return () => clearInterval(iv) }, [syncing, loadSources, loadDocs])
  const patch = async (s: KbSource, p: Parameters<typeof kb.patchSource>[1], msg?: string) => { try { const n = await kb.patchSource(s.id, p); setSources((a) => a?.map((x) => (x.id === s.id ? n : x)) ?? null); if (msg) toast(msg) } catch (e) { toast(errMsg(e)) } }
  const sync = async (s: KbSource) => { try { await kb.sync(s.id); toast(t("knowledge:sources.syncStarted")); setTimeout(() => void loadSources(), 300) } catch (e) { toast(errMsg(e)) } }
  const remove = async (s: KbSource) => { try { await kb.deleteSource(s.id); toast(t("knowledge:sources.removed")); void loadSources(); void loadDocs() } catch (e) { toast(errMsg(e)) } }

  const all = t("knowledge:all")
  const names = [all, ...new Set((docs ?? []).map((d) => d.source.name))]
  const shown = (docs ?? []).filter((d) => !src || d.source.name === src)

  return (
    <div className="flex flex-col gap-6">
      <section data-materials-docs>
        <SectionLabel right={<span className="flex flex-wrap justify-end gap-2">
          <AddFiles onDone={() => { void loadSources(); void loadDocs() }} />
          <Button size="sm" variant="outline" onClick={() => setFolder(true)} data-add-folder><FolderPlusIcon />{t("knowledge:sources.addFolder")}</Button>
          <Button size="sm" onClick={() => setNewNote(true)} data-new-note><FilePlusIcon />{t("knowledge:docs.newNote")}</Button>
        </span>}>{t("knowledge:tab.docs")}</SectionLabel>
        {names.length > 2 && <div className="mb-3"><Chips value={src && names.includes(src) ? src : all} onChange={(x) => setSrc(x === all ? "" : x)} options={names} /></div>}
        {docErr ? <Empty title={t("knowledge:docs.failed")} sub={docErr} /> : !docs ? <Loading /> : !shown.length ? <Empty title={t("knowledge:docs.noneTitle")} sub={t("knowledge:docs.noneSub")} /> :
          <Group>{shown.map((d) => (
            <Row key={d.id} onClick={() => go(docHref(d))} data-doc={d.id}
              title={<span className="truncate">{d.title}</span>}
              sub={<span className="flex flex-wrap gap-x-1.5"><span>{d.source.name}</span>{d.source.kind !== "notes" && d.ref && <span className="truncate">· {d.ref}</span>}<span>· {visLabel(d.visibility)}</span><span>· {tm(d.updated)}</span>{d.status !== "active" && <span className="text-attention">· {d.status === "error" ? t("knowledge:docs.error") : d.status}</span>}</span>}>
              <LevelTag l={d.level} />
            </Row>))}</Group>}
      </section>

      <section data-materials-sources>
        <SectionLabel>{t("knowledge:sources.docsTitle")}</SectionLabel>
        {srcErr ? <Empty title={t("knowledge:sources.failed")} sub={srcErr} /> : !sources ? <Loading /> :
          <Panel>{sources.map((s) => { const [dot, state] = sourceState(s); return (
            <Row key={s.id} data-source={s.id} data-source-kind={s.kind} data-source-state={s.paused ? "paused" : s.status} className={cn(STACK, s.paused && "opacity-70")}
              title={<span className="inline-flex flex-wrap items-center gap-x-2 gap-y-0.5">{s.name}<span className="type-footnote text-muted-foreground">{SOURCE_KIND[s.kind] ?? s.kind}</span></span>}
              sub={<span className="flex flex-col gap-0.5"><span className="inline-flex flex-wrap items-center gap-x-1.5"><span className="inline-flex items-center gap-1" data-source-status><StatusDot s={dot} />{state}</span><span aria-hidden>·</span><span data-source-stats>{statsLine(s)}</span></span>{s.path && <span className="truncate font-mono">{s.path.replace(/^\/Users\/[^/]+/, "~")}</span>}{s.error && <span className="break-words text-destructive" data-source-error>{s.error}</span>}</span>}>
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
            </Row>) })}</Panel>}
        <div className="mt-2 px-1 type-footnote text-muted-foreground">{t("knowledge:sources.footer")}</div>
      </section>

      {prov && <MemProviders prov={prov} />}
      <NewNoteDialog open={newNote} onClose={() => setNewNote(false)} />
      <FolderDialog open={folder} onClose={() => setFolder(false)} onDone={() => { void loadSources(); void loadDocs() }} />
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

const PROVIDER_ICON: Record<string, typeof UserIcon> = { you: UserIcon, conversation: MessagesSquareIcon, item: FileTextIcon, agent: BotIcon, mail: MailIcon, space: UsersIcon, event: ZapIcon, plugin: PuzzleIcon }
const providerName = (p: MemProvider) => {
  const k = ["you", "conversation", "item", "agent", "mail", "space", "event", "plugin"].includes(p.kind) ? p.kind : "other"
  return tt(`knowledge:sources.provider.${k as "you"}`)
}
function MemProviders({ prov }: { prov: { memory: MemProvider[]; store: string } }) {
  const t = useT()
  return (
    <section data-mem-providers>
      <SectionLabel>{t("knowledge:sources.memTitle")}</SectionLabel>
      {!prov.memory.length ? <Blank>{t("knowledge:sources.memNone")}</Blank> :
        <Panel>{prov.memory.map((p) => (
          <Row key={p.kind + p.writer} data-provider={p.kind} icon={<Tile I={PROVIDER_ICON[p.kind] ?? SparklesIcon} />}
            title={<span className="inline-flex flex-wrap items-baseline gap-x-2">{providerName(p)}{p.writer_name && p.kind !== "you" && <span className="type-footnote text-muted-foreground">{t("knowledge:origin.by", { name: p.writer_name })}</span>}</span>}
            sub={<Meta inline><span>{t("knowledge:sources.memCount", { count: p.count })}</span>{p.candidates > 0 && <span className="text-attention">{t("knowledge:subject.pending", { count: p.candidates })}</span>}{p.stale > 0 && <span>{t("knowledge:sources.memStale", { count: p.stale })}</span>}{p.last > 0 && <span>{t("knowledge:sources.memLast", { time: tm(p.last) })}</span>}</Meta>}>
            <span className="type-callout tabular-nums text-muted-foreground">{p.confirmed}</span>
          </Row>))}</Panel>}
      <div className="mt-2 px-1 type-footnote text-muted-foreground" data-kb-store={prov.store}>{t(prov.store === "openviking" ? "knowledge:sources.storeOv" : "knowledge:sources.storeBuiltin")} {t("knowledge:sources.memFooter")}</div>
    </section>
  )
}

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

/* ───────── 待确认（仅知识候选） ───────── */
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
  const ids = xs.slice(0, 50).map((c) => c.mem.id)
  return (
    <section>
      <SectionLabel right={<span className="flex gap-1"><Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void resolve(ids, "no")} data-cand-all-no>{t("knowledge:cand.ignoreAll")}</Button><Button size="xs" variant="outline" disabled={busy} onClick={() => void resolve(ids, "yes")} data-cand-all-yes>{t("knowledge:cand.confirmAll")}</Button></span>}>
        {t("knowledge:cand.title", { count: xs.length })}</SectionLabel>
      <Group>{xs.map((c) => (
        <Row key={c.mem.id} data-candidate={c.mem.id} className={STACK}
          title={<span className="inline-flex flex-wrap items-center gap-2">{c.mem.text}<Badge variant="secondary" className="text-muted-foreground" data-cand-record>{c.record}</Badge></span>}
          sub={<span className="flex flex-col gap-0.5"><span data-cand-origin>{memKind(c.mem.kind)}<span aria-hidden> · </span><OriginLine m={c.origin_label && !c.mem.origin ? { ...c.mem, source_label: c.origin_label } : c.mem} /></span>{c.why && <span className="break-words text-foreground/80" data-cand-why>{t("knowledge:cand.why", { why: c.why })}</span>}{c.scope && <span className="break-words">{t("knowledge:cand.scope", { scope: c.scope })}</span>}{c.evidence && <span className="break-words">{t("knowledge:cand.evidence", { evidence: c.evidence })}</span>}{c.supersedes && <span className="break-words">{t("knowledge:cand.supersedes", { text: c.supersedes.text })}</span>}</span>}>
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
