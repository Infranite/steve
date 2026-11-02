/* 知识 (knowledge.md §9): search, memories, notes and documents, sources, and candidates waiting for you.
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
import { kb, levelLabel, visLabel, memKind, MEM_KINDS, SOURCE_KIND, LEVEL_DOT, type Candidate, type DocBrief, type Hit, type KbSource, type Mem } from "@/api/knowledge"
import { KbGate, LevelSel, VisSel, errMsg } from "@/knowledge/fields"
import { DocView, NoteView } from "@/knowledge/doc"

type Tab = "搜索" | "记忆" | "笔记与文档" | "来源" | "待确认"
let tab: Tab = "记忆", query = ""

/** `#/knowledge`, `#/knowledge?mem=…`, `#/knowledge/doc/:id?seq=`, `#/knowledge/note/:id` */
export function KnowledgeView({ sub, id }: { sub?: string; id?: string }) {
  const [docId] = (id ?? "").split("?")
  return <KbGate>{sub === "doc" && docId ? <DocView id={docId} /> : sub === "note" && docId ? <NoteView id={docId} /> : <KnowledgeHome />}</KbGate>
}

const hashParam = (k: string) => new URLSearchParams(location.hash.split("?")[1] ?? "").get(k)

function KnowledgeHome() {
  const [, f] = useState(0), r = () => f((x) => x + 1)
  const focusMem = hashParam("mem")
  if (focusMem && tab !== "记忆") tab = "记忆"
  const [pend, setPend] = useState(0)
  const loadPend = useCallback(() => { kb.candidates().then((xs) => setPend(xs.length), () => {}) }, [])
  useEffect(loadPend, [loadPend])
  useHubEvents(["mm", "kb"], loadPend)
  const setTab = (t: Tab) => { tab = t; r() }
  const opts = [["搜索", "搜索"], ["记忆", "记忆"], ["笔记与文档", "笔记与文档"], ["来源", "来源"],
    ["待确认", <span key="p" className="inline-flex items-center gap-1">待确认{pend > 0 && <span className="rounded-full bg-attention px-1.5 text-[10px] font-semibold text-white" data-pending-count>{pend}</span>}</span>, "待确认"]] as const
  return (
    <Page title="知识" sub="Steve 记得的事和能查到的资料" max={760}>
      <div className="flex flex-col gap-5">
        <SearchBox onQuery={(q) => { query = q; if (q && tab !== "搜索") tab = "搜索"; r() }} />
        <div className="-mx-1 overflow-x-auto px-1 pb-0.5 [scrollbar-width:none]"><Segmented value={tab} onChange={setTab} options={opts} /></div>
        <div data-kb-tab={tab}>
          {tab === "搜索" && <SearchTab q={query} />}
          {tab === "记忆" && <MemsTab focus={focusMem} />}
          {tab === "笔记与文档" && <DocsTab />}
          {tab === "来源" && <SourcesTab />}
          {tab === "待确认" && <CandidatesTab onChange={setPend} />}
        </div>
      </div>
    </Page>
  )
}

function SearchBox({ onQuery }: { onQuery: (q: string) => void }) {
  const [v, setV] = useState(query), t = useRef<number>(0)
  const set = (x: string) => { setV(x); clearTimeout(t.current); t.current = window.setTimeout(() => onQuery(x.trim()), 250) }
  return (
    <div role="search" className="flex items-center gap-2 rounded-xl border bg-card px-3 focus-within:border-blue/60">
      <SearchIcon className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.75} />
      <input type="search" aria-label="搜索知识库" value={v} onChange={(e) => set(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter") { clearTimeout(t.current); onQuery(v.trim()) } }}
        placeholder="搜索记忆、笔记和文档" maxLength={200} className="h-10 min-w-0 flex-1 bg-transparent text-[14px] outline-none placeholder:text-tertiary [&::-webkit-search-cancel-button]:hidden" data-kb-search />
      {v && <Button variant="ghost" size="icon-xs" aria-label="清除" className="text-muted-foreground" onClick={() => { setV(""); onQuery("") }}><XIcon /></Button>}
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
  const [res, setRes] = useState<{ q: string; hits: Hit[]; ms: number } | null>(null), [err, setErr] = useState("")
  useEffect(() => {
    if (!q) { setRes(null); return }
    let on = true; setErr("")
    kb.search(q).then((x) => on && setRes({ q, hits: x.hits, ms: x.took_ms }), (e) => on && setErr(errMsg(e)))
    return () => { on = false }
  }, [q])
  if (!q) return <Empty icon={<SearchIcon />} title="在知识库里找" sub="记忆、笔记、文件夹和事项里的内容都能搜到。" />
  if (err) return <Empty title="搜索失败" sub={err} />
  if (!res || res.q !== q) return <div className="flex justify-center py-12" role="status" aria-label="正在搜索"><Spinner className="size-5 text-muted-foreground" /></div>
  if (!res.hits.length) return <Empty title="没有找到" sub={`知识库里没有和“${q}”相关的内容。`} />
  return (
    <section aria-label="搜索结果"><SectionLabel>{res.hits.length} 条结果 · {res.ms} ms</SectionLabel>
      <Group>{res.hits.map((h) => (
        <Row key={h.type + h.id + h.seq} onClick={() => go(h.href)} data-hit={h.id}
          title={<span className="flex min-w-0 items-baseline gap-2"><span className="min-w-0 truncate"><Highlight text={h.title} q={q} />{h.heading ? <span className="text-muted-foreground"> · <Highlight text={h.heading} q={q} /></span> : null}</span></span>}
          sub={<span className="flex flex-col gap-1"><span className="line-clamp-3 type-callout text-foreground/80"><Highlight text={h.text} q={q} /></span><span className="truncate">{h.label}</span></span>}>
          <LevelTag l={h.level} />
        </Row>))}</Group>
    </section>
  )
}

/* ───────── 记忆 ───────── */
const aboutKey = (m: Mem) => m.about.map((a) => a.type + ":" + a.id).sort().join(",")
const aboutLabel = (m: Mem) => (m.about.length ? "关于 " + m.about.map((a) => a.name ?? (a.type === "party" ? "联系人 " : a.type === "item" ? "事项 " : "") + a.id).join("、") : "通用")

function MemsTab({ focus }: { focus: string | null }) {
  const [xs, setXs] = useState<Mem[] | null>(null), [err, setErr] = useState(""), [kind, setKind] = useState("全部"), [v, setV] = useState(""), [busy, setBusy] = useState(false)
  const load = useCallback(() => kb.mems().then(setXs, (e) => setErr(errMsg(e))), [])
  useEffect(() => { void load() }, [load])
  useHubEvents(["mm"], () => void load())
  useEffect(() => { if (focus && xs) document.querySelector(`[data-mem="${CSS.escape(focus)}"]`)?.scrollIntoView({ block: "center" }) }, [focus, xs])
  const add = async () => {
    const t = v.trim(); if (!t || busy) return
    setBusy(true)
    try { await kb.addMem({ text: t }); setV(""); toast("已记下"); await load() } catch (e) { toast(errMsg(e)) } finally { setBusy(false) }
  }
  const patch = async (m: Mem, p: Parameters<typeof kb.patchMem>[1]) => { try { const n = await kb.patchMem(m.id, p); setXs((a) => a?.map((x) => (x.id === m.id ? { ...x, ...n } : x)) ?? null) } catch (e) { toast(errMsg(e)) } }
  const remove = async (m: Mem) => { try { await kb.deleteMem(m.id); setXs((a) => a?.filter((x) => x.id !== m.id) ?? null); toast("已删除") } catch (e) { toast(errMsg(e)) } }
  const kinds = ["全部", ...MEM_KINDS.filter(([k]) => xs?.some((m) => m.kind === k)).map(([, l]) => l)]
  const shown = (xs ?? []).filter((m) => kind === "全部" || memKind(m.kind) === kind)
  const groups = new Map<string, Mem[]>(); for (const m of shown) { const k = aboutKey(m); groups.set(k, [...(groups.get(k) ?? []), m]) }
  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center gap-2 rounded-xl border bg-card py-1 pl-3 pr-1 focus-within:border-blue/60">
        <input aria-label="添加一条记忆" value={v} onChange={(e) => setV(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && !e.nativeEvent.isComposing) void add() }} maxLength={2000}
          placeholder="添加一条记忆，例如：周五下午不安排部署" className="h-8 min-w-0 flex-1 bg-transparent text-[14px] outline-none placeholder:text-tertiary" data-mem-input />
        <Button size="sm" disabled={!v.trim() || busy} onClick={() => void add()} data-mem-add>{busy ? <Spinner className="size-3.5" /> : <PlusIcon />}添加</Button>
      </div>
      {kinds.length > 2 && <Chips value={kinds.includes(kind) ? kind : "全部"} onChange={setKind} options={kinds} />}
      {err ? <Empty title="记忆打不开" sub={err} /> : !xs ? <Loading /> : !shown.length ? <Empty title="还没有记忆" sub="你说的、Steve 从事项和对话里学到并经你确认的事，都记在这里。" /> :
        [...groups.entries()].map(([k, ms]) => (
          <section key={k}><SectionLabel>{aboutLabel(ms[0])} · {ms.length}</SectionLabel>
            <Group>{ms.map((m) => (
              <Row key={m.id} data-mem={m.id} className={cn(STACK, m.id === focus && "bg-blue/5", m.status === "stale" && "opacity-60")}
                title={<span className="inline-flex flex-wrap items-center gap-2">{m.text}{m.status === "candidate" && <Badge variant="outline" className="font-normal text-attention">待确认</Badge>}{m.status === "stale" && <Badge variant="outline" className="font-normal text-muted-foreground">已过时</Badge>}</span>}
                sub={`${memKind(m.kind)} · 来源：${m.source_label || "你写的"}`}>
                <span className="flex flex-wrap items-center justify-end gap-0.5 max-sm:justify-start">
                  <LevelSel v={m.level} on={(l) => l && void patch(m, { level: l })} testId={"mem-level-" + m.id} />
                  <VisSel v={m.visibility} on={(x) => void patch(m, { visibility: x })} testId={"mem-vis-" + m.id} />
                  <Button variant="ghost" size="icon-xs" aria-label={"删除记忆：" + m.text} className="text-muted-foreground" onClick={() => void remove(m)} data-mem-del><XIcon /></Button>
                </span>
              </Row>))}</Group>
          </section>))}
      <div className="-mt-2 px-1 type-footnote text-muted-foreground">披露级别决定回复里能说到什么程度：“仅用于推理”只帮 Steve 判断，不会被说出来；“不可使用”的不会进入任何回答。</div>
    </div>
  )
}
/** on a phone the row's controls go on their own line under the text */
const STACK = "max-sm:[&>[data-slot=item-actions]]:w-full max-sm:[&>[data-slot=item-actions]]:justify-start max-sm:[&>[data-slot=item-actions]]:-ml-2"
const Loading = () => <div className="flex justify-center py-12" role="status" aria-label="正在加载"><Spinner className="size-5 text-muted-foreground" /></div>

/* ───────── 笔记与文档 ───────── */
export const docHref = (d: { id: string; source: { kind: string } }) => (d.source.kind === "notes" ? "#/knowledge/note/" : "#/knowledge/doc/") + d.id
function DocsTab() {
  const [xs, setXs] = useState<DocBrief[] | null>(null), [err, setErr] = useState(""), [src, setSrc] = useState("全部"), [newNote, setNewNote] = useState(false)
  const loadDocs = useCallback(() => kb.docs().then(setXs, (e) => setErr(errMsg(e))), [])
  useEffect(() => { void loadDocs() }, [loadDocs])
  useHubEvents(["kb"], () => void loadDocs())
  const names = ["全部", ...new Set((xs ?? []).map((d) => d.source.name))]
  const shown = (xs ?? []).filter((d) => src === "全部" || d.source.name === src)
  return (
    <div className="flex flex-col gap-5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        {names.length > 2 ? <Chips value={names.includes(src) ? src : "全部"} onChange={setSrc} options={names} /> : <span />}
        <Button size="sm" onClick={() => setNewNote(true)} data-new-note><FilePlusIcon />新建笔记</Button>
      </div>
      {err ? <Empty title="文档打不开" sub={err} /> : !xs ? <Loading /> : !shown.length ? <Empty title="还没有笔记和文档" sub="新建一篇笔记，或者在“来源”里添加文件夹和文件。" /> :
        <Group>{shown.map((d) => (
          <Row key={d.id} onClick={() => go(docHref(d))} data-doc={d.id}
            title={<span className="truncate">{d.title}</span>}
            sub={<span className="flex flex-wrap gap-x-1.5"><span>{d.source.name}</span>{d.source.kind !== "notes" && d.ref && <span className="truncate">· {d.ref}</span>}<span>· {visLabel(d.visibility)}</span><span>· {tm(d.updated)}</span>{d.status !== "active" && <span className="text-attention">· {d.status === "error" ? "出错" : d.status}</span>}</span>}>
            <LevelTag l={d.level} />
          </Row>))}</Group>}
      <NewNoteDialog open={newNote} onClose={() => setNewNote(false)} />
    </div>
  )
}

function NewNoteDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [title, setTitle] = useState(""), [body, setBody] = useState(""), [busy, setBusy] = useState(false)
  const save = async () => {
    if (!title.trim() || busy) return
    setBusy(true)
    try { const d = await kb.newNote({ title: title.trim(), body }); toast("已保存，现在就能搜到"); onClose(); go("#/knowledge/note/" + d.id) } catch (e) { toast(errMsg(e)) } finally { setBusy(false) }
  }
  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[560px]">
        <DialogHeader><DialogTitle className="type-headline">新建笔记</DialogTitle><DialogDescription className="type-callout">保存后马上进入知识库，默认只有你能看到。</DialogDescription></DialogHeader>
        <FieldGroup className="gap-4">
          <Field><FieldLabel htmlFor="kb-note-title">标题</FieldLabel><Input id="kb-note-title" autoFocus maxLength={120} value={title} onChange={(e) => setTitle(e.target.value)} placeholder="例如：Acme 合同说明" data-note-title /></Field>
          <Field><FieldLabel htmlFor="kb-note-body">正文</FieldLabel><Textarea id="kb-note-body" className="min-h-40 font-mono text-[13px]" value={body} onChange={(e) => setBody(e.target.value)} placeholder="支持 Markdown" data-note-body
            onKeyDown={(e) => { if ((e.metaKey || e.ctrlKey) && e.key === "Enter") void save() }} /></Field>
        </FieldGroup>
        <DialogFooter className="gap-2"><Button variant="ghost" onClick={onClose}>取消</Button><Button disabled={!title.trim() || busy} onClick={() => void save()} data-note-save>{busy && <Spinner className="size-3.5" />}保存</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/* ───────── 来源 ───────── */
const statsLine = (s: KbSource) => {
  const x = s.stats ?? {}, parts = [`${x.docs ?? 0} 篇`, `${x.chunks ?? 0} 段`]
  if (x.skipped) parts.push(`跳过 ${x.skipped}`)
  /* built-in sources follow their objects as they change; only folders are scanned */
  if (s.status === "syncing") parts.push("正在同步…"); else if (s.last_sync) parts.push("上次同步 " + tm(s.last_sync)); else if (!s.builtin) parts.push("还没同步")
  return parts.join(" · ")
}
function SourcesTab() {
  const [xs, setXs] = useState<KbSource[] | null>(null), [err, setErr] = useState(""), [folder, setFolder] = useState(false)
  const load = useCallback(() => kb.sources().then(setXs, (e) => setErr(errMsg(e))), [])
  useEffect(() => { void load() }, [load])
  useHubEvents(["kb"], () => void load())
  /* a scan runs in the background: poll while one is going */
  const syncing = xs?.some((s) => s.status === "syncing")
  useEffect(() => { if (!syncing) return; const t = setInterval(() => void load(), 2000); return () => clearInterval(t) }, [syncing, load])
  const patch = async (s: KbSource, p: Parameters<typeof kb.patchSource>[1], msg?: string) => { try { const n = await kb.patchSource(s.id, p); setXs((a) => a?.map((x) => (x.id === s.id ? n : x)) ?? null); if (msg) toast(msg) } catch (e) { toast(errMsg(e)) } }
  const sync = async (s: KbSource) => { try { await kb.sync(s.id); toast("开始同步"); setTimeout(() => void load(), 300) } catch (e) { toast(errMsg(e)) } }
  const remove = async (s: KbSource) => { try { await kb.deleteSource(s.id); toast("已移除来源"); void load() } catch (e) { toast(errMsg(e)) } }
  return (
    <div className="flex flex-col gap-5">
      <div className="flex flex-wrap justify-end gap-2"><AddFiles onDone={() => void load()} /><Button size="sm" onClick={() => setFolder(true)} data-add-folder><FolderPlusIcon />添加文件夹</Button></div>
      {err ? <Empty title="来源打不开" sub={err} /> : !xs ? <Loading /> :
        <Group>{xs.map((s) => (
          <Row key={s.id} data-source={s.id} data-source-kind={s.kind} className={cn(STACK, s.paused && "opacity-70")}
            title={<span className="inline-flex flex-wrap items-center gap-2">{s.name}<span className="type-footnote text-muted-foreground">{SOURCE_KIND[s.kind] ?? s.kind}</span>{s.status === "error" && <Badge variant="outline" className="font-normal text-destructive">出错</Badge>}</span>}
            sub={<span className="flex flex-col"><span data-source-stats>{statsLine(s)}</span>{s.path && <span className="truncate font-mono text-[11.5px]">{s.path.replace(/^\/Users\/[^/]+/, "~")}</span>}{s.error && <span className="text-destructive">{s.error}</span>}</span>}>
            <span className="flex flex-wrap items-center justify-end gap-0.5 max-sm:justify-start">
              <LevelSel v={s.level} on={(l) => l && void patch(s, { level: l }, "默认级别已改，已有内容跟着变")} testId={"src-level-" + s.id} />
              <VisSel v={s.visibility} on={(x) => void patch(s, { visibility: x })} testId={"src-vis-" + s.id} />
              <Switch checked={!s.paused} onCheckedChange={(on) => void patch(s, { paused: !on }, on ? "已恢复" : "已暂停：不同步，也不参与检索")} aria-label={(s.paused ? "恢复 " : "暂停 ") + s.name} className="ml-1" data-source-on />
              {s.kind === "folder" && <DropdownMenu>
                <DropdownMenuTrigger render={<Button variant="ghost" size="icon-xs" aria-label={"更多：" + s.name} className="text-muted-foreground" data-source-menu />}><MoreHorizontalIcon /></DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="min-w-36">
                  <DropdownMenuItem disabled={s.status === "syncing" || s.paused} onClick={() => void sync(s)} data-source-sync><RefreshCwIcon />立即同步</DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem variant="destructive" onClick={() => void remove(s)} data-source-del><TrashIcon />移除来源</DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>}
            </span>
          </Row>))}</Group>}
      <div className="-mt-2 px-1 type-footnote text-muted-foreground">新内容默认用来源的级别和可见范围，单篇可以在文档里改。暂停的来源不同步，也不参与检索。</div>
      <FolderDialog open={folder} onClose={() => setFolder(false)} onDone={() => void load()} />
    </div>
  )
}

/** "添加文件": the composer's upload path, then each uploaded file joins the knowledge base */
function AddFiles({ onDone }: { onDone: () => void }) {
  const up = useUploads(), input = useRef<HTMLInputElement>(null), added = useRef(new Set<string>())
  useEffect(() => {
    const fresh = up.ready.filter((f) => !added.current.has(f.id)); if (!fresh.length) return
    for (const f of fresh) added.current.add(f.id)
    Promise.allSettled(fresh.map((f) => kb.addFile(f.id))).then((rs) => {
      const bad = rs.find((r) => r.status === "rejected") as PromiseRejectedResult | undefined
      toast(bad ? errMsg(bad.reason) : `已加入知识库：${fresh.map((f) => f.name).join("、")}`)
      if (!up.uploading) up.clear()
      onDone()
    })
  }, [up.ready.length]) // eslint-disable-line react-hooks/exhaustive-deps
  const failed = up.list.find((p) => p.error)
  useEffect(() => { if (failed) { toast(`${failed.name}：${failed.error}`); up.remove(failed.key) } }, [failed?.key]) // eslint-disable-line react-hooks/exhaustive-deps
  return <>
    <input ref={input} type="file" multiple hidden accept=".md,.txt,.html,.pdf,.docx,text/*,application/pdf" onChange={(e) => { if (e.target.files) up.add(e.target.files); e.target.value = "" }} data-add-file-input />
    <Button size="sm" variant="outline" disabled={up.uploading} onClick={() => input.current?.click()} data-add-file>{up.uploading ? <Spinner className="size-3.5" /> : <UploadIcon />}添加文件</Button>
  </>
}

const lines = (s: string) => s.split(/[\n,]/).map((x) => x.trim()).filter(Boolean)
function FolderDialog({ open, onClose, onDone }: { open: boolean; onClose: () => void; onDone: () => void }) {
  const [name, setName] = useState(""), [path, setPath] = useState(""), [inc, setInc] = useState("**/*.md\n**/*.txt"), [exc, setExc] = useState(".obsidian/**\nnode_modules/**"), [busy, setBusy] = useState(false), [err, setErr] = useState("")
  useEffect(() => { if (open) { setErr(""); setBusy(false) } }, [open])
  const save = async () => {
    if (!path.trim() || busy) return
    setBusy(true); setErr("")
    const p = path.trim(), n = name.trim() || p.replace(/\/+$/, "").split("/").pop() || "文件夹"
    try { await kb.addFolder({ name: n.slice(0, 40), path: p, include: lines(inc), exclude: lines(exc) }); toast("已添加，正在扫描"); setName(""); setPath(""); onClose(); onDone() }
    catch (e) { setErr(errMsg(e)) } finally { setBusy(false) }
  }
  const field = (id: string, label: ReactNode, el: ReactNode, desc?: string) => <Field><FieldLabel htmlFor={id}>{label}</FieldLabel>{el}{desc && <FieldDescription>{desc}</FieldDescription>}</Field>
  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[520px]">
        <DialogHeader><DialogTitle className="type-headline">添加文件夹</DialogTitle><DialogDescription className="type-callout">Steve 会读这个文件夹里的文档，改动后自动重新整理。文件不会被修改。</DialogDescription></DialogHeader>
        <FieldGroup className="gap-4">
          {field("kb-folder-path", "路径", <Input id="kb-folder-path" autoFocus className="font-mono" value={path} onChange={(e) => setPath(e.target.value)} placeholder="/Users/user/Documents/wiki" data-folder-path />, "Hub 所在电脑上的目录")}
          {field("kb-folder-name", "名字", <Input id="kb-folder-name" maxLength={40} value={name} onChange={(e) => setName(e.target.value)} placeholder="默认用文件夹名" data-folder-name />)}
          <div className="grid gap-4 sm:grid-cols-2">
            {field("kb-folder-inc", "包含", <Textarea id="kb-folder-inc" className="min-h-20 font-mono text-[12.5px]" value={inc} onChange={(e) => setInc(e.target.value)} data-folder-include />, "每行一个规则")}
            {field("kb-folder-exc", "排除", <Textarea id="kb-folder-exc" className="min-h-20 font-mono text-[12.5px]" value={exc} onChange={(e) => setExc(e.target.value)} data-folder-exclude />)}
          </div>
          {err && <div role="alert" className="type-callout text-destructive" data-folder-error>{err}</div>}
        </FieldGroup>
        <DialogFooter className="gap-2"><Button variant="ghost" onClick={onClose}>取消</Button><Button disabled={!path.trim() || busy} onClick={() => void save()} data-folder-save>{busy && <Spinner className="size-3.5" />}添加</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/* ───────── 待确认 ───────── */
function CandidatesTab({ onChange }: { onChange: (n: number) => void }) {
  const [xs, setXs] = useState<Candidate[] | null>(null), [err, setErr] = useState(""), [busy, setBusy] = useState(false)
  const load = useCallback(() => kb.candidates().then((a) => { setXs(a); onChange(a.length) }, (e) => setErr(errMsg(e))), [onChange])
  useEffect(() => { void load() }, [load])
  useHubEvents(["mm"], () => void load())
  const resolve = async (ids: string[], choice: "yes" | "no") => {
    if (!ids.length || busy) return
    setBusy(true)
    try { const r = await kb.resolve(ids, choice); toast(choice === "yes" ? `已确认 ${r.done} 条` : `已忽略 ${r.done} 条`); await load() } catch (e) { toast(errMsg(e)) } finally { setBusy(false) }
  }
  if (err) return <Empty title="待确认打不开" sub={err} />
  if (!xs) return <Loading />
  if (!xs.length) return <Empty title="没有待确认的记忆" sub="Steve 从事项和对话里学到的事，会先放在这里等你确认。" />
  /* the batch call takes at most 50 ids */
  const ids = xs.slice(0, 50).map((c) => c.mem.id)
  return (
    <section>
      <SectionLabel right={<span className="flex gap-1"><Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void resolve(ids, "no")} data-cand-all-no>全部忽略</Button><Button size="xs" variant="outline" disabled={busy} onClick={() => void resolve(ids, "yes")} data-cand-all-yes>全部确认</Button></span>}>
        待确认 · {xs.length}</SectionLabel>
      <Group>{xs.map((c) => (
        <Row key={c.mem.id} data-candidate={c.mem.id} className={STACK}
          title={<span className="inline-flex flex-wrap items-center gap-2">{c.mem.text}<Badge variant="outline" className="font-normal text-attention">待确认</Badge></span>}
          sub={<span className="flex flex-col gap-0.5"><span>{memKind(c.mem.kind)} · 来源：{c.origin_label || c.mem.source_label}</span>{c.evidence && <span className="break-words">原话：{c.evidence}</span>}{c.supersedes && <span className="break-words">会替换：{c.supersedes.text}</span>}</span>}>
          <span className="flex items-center gap-1">
            <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void resolve([c.mem.id], "no")} data-cand-no>不对</Button>
            <Button size="xs" variant="outline" disabled={busy} onClick={() => void resolve([c.mem.id], "yes")} data-cand-yes>确认</Button>
          </span>
        </Row>))}</Group>
      <div className="mt-2 px-1 type-footnote text-muted-foreground">确认后才会用到；30 天没处理的会自动过期。</div>
    </section>
  )
}

