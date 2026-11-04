/* A document (`#/knowledge/doc/:id?seq=`) and a note (`#/knowledge/note/:id`): properties, the chunks the
 * index holds, and for notes the editor. */
import { useEffect, useRef, useState, type ReactNode } from "react"
import { cn } from "cn"
import { TrashIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Spinner } from "@/components/ui/spinner"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { Markdown } from "@/components/markdown"
import { Page, SectionLabel, Segmented, Empty, Meta, go } from "@/ui"
import { toast, tm } from "@/store"
import { kb, SOURCE_KIND, visLabel, levelLabel, type Doc, type Level } from "@/api/knowledge"
import { LevelSel, VisSel, errMsg } from "@/knowledge/fields"

const BACK: [string, string] = ["#/knowledge", "知识"]
const seqOf = () => { const v = new URLSearchParams(location.hash.split("?")[1] ?? "").get("seq"); return v === null ? null : Number(v) }

function useDoc(id: string) {
  const [d, setD] = useState<Doc | null>(null), [err, setErr] = useState("")
  useEffect(() => { let on = true; kb.doc(id).then((x) => on && setD(x), (e) => on && setErr(errMsg(e))); return () => { on = false } }, [id])
  return { d, setD, err }
}

function Prop({ label, children }: { label: string; children: ReactNode }) {
  return <div className="flex min-h-9 items-center gap-3 border-t py-1 first:border-t-0"><span className="w-24 shrink-0 type-footnote text-muted-foreground">{label}</span><div className="flex min-w-0 flex-1 items-center justify-end gap-2 text-right type-callout sm:justify-start sm:text-left">{children}</div></div>
}

/** source, path, level, visibility, about, uses; level and visibility can go back to following the source */
function Props({ d, setD }: { d: Doc; setD: (d: Doc) => void }) {
  const [src, setSrc] = useState<{ level: Level; visibility: string } | null>(null)
  useEffect(() => { kb.sources().then((xs) => { const s = xs.find((x) => x.id === d.source.id); if (s) setSrc({ level: s.level, visibility: s.visibility }) }, () => {}) }, [d.source.id])
  const patch = async (p: Parameters<typeof kb.patchDoc>[1], msg: string) => { try { setD(await kb.patchDoc(d.id, p)); toast(msg) } catch (e) { toast(errMsg(e)) } }
  return (
    <section aria-label="属性" className="rounded-xl border px-4 py-1.5" data-doc-props>
      <Prop label="来源"><span>{d.source.name}<span className="text-muted-foreground"> · {SOURCE_KIND[d.source.kind] ?? d.source.kind}</span></span></Prop>
      {d.source.kind !== "notes" && d.ref && <Prop label="路径"><span className="truncate font-mono text-[12.5px]">{d.ref}</span></Prop>}
      <Prop label="披露级别"><LevelSel v={d.inherited.level ? "" : d.level} inherit={src?.level ?? d.level} on={(l) => void patch({ level: l || null }, l ? `已改成“${levelLabel(l)}”` : "已改回跟随来源")} testId="doc-level" /></Prop>
      <Prop label="可见范围"><VisSel v={d.inherited.visibility ? "" : d.visibility} inherit={src?.visibility ?? d.visibility} on={(v) => void patch({ visibility: v || null }, v ? `已改成“${visLabel(v)}”` : "已改回跟随来源")} testId="doc-vis" /></Prop>
      {d.about.length > 0 && <Prop label="关于"><span className="flex flex-wrap gap-1">{d.about.map((a) => <span key={a.type + a.id} className="inline-flex h-6 items-center rounded-full bg-muted px-2 type-footnote">{a.name ?? a.id}</span>)}</span></Prop>}
      <Prop label="使用"><span className="text-muted-foreground" data-doc-uses>被用到 {d.uses} 次 · {d.chunks} 段 · 更新于 {tm(d.updated) || "—"}</span></Prop>
      {d.error && <Prop label="出错"><span className="text-destructive">{d.error}</span></Prop>}
    </section>
  )
}

function DeleteButton({ d, what }: { d: Doc; what: string }) {
  const [open, setOpen] = useState(false)
  const del = async () => { try { await kb.deleteDoc(d.id); toast(what === "笔记" ? "笔记已删除" : "已从知识库移除"); go("#/knowledge") } catch (e) { toast(errMsg(e)) } }
  return <>
    <Button size="sm" variant="ghost" className="text-destructive" onClick={() => setOpen(true)} data-doc-delete><TrashIcon />{what === "笔记" ? "删除" : "移除"}</Button>
    <AlertDialog open={open} onOpenChange={setOpen}><AlertDialogContent>
      <AlertDialogHeader><AlertDialogTitle>{what === "笔记" ? `删除“${d.title}”？` : `从知识库移除“${d.title}”？`}</AlertDialogTitle>
        <AlertDialogDescription>{what === "笔记" ? "笔记和它的片段一起删除，Steve 之后查不到它。" : "原文件不受影响；Steve 之后查不到它。"}</AlertDialogDescription></AlertDialogHeader>
      <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => void del()} data-doc-delete-ok>{what === "笔记" ? "删除" : "移除"}</AlertDialogAction></AlertDialogFooter>
    </AlertDialogContent></AlertDialog>
  </>
}

/** the indexed chunks; `?seq=` scrolls to that one and marks it */
function Chunks({ d }: { d: Doc }) {
  const seq = seqOf(), box = useRef<HTMLDivElement>(null)
  useEffect(() => { if (seq !== null) box.current?.querySelector(`[data-seq="${seq}"]`)?.scrollIntoView({ block: "center" }) }, [seq, d.id])
  if (!d.preview.length) return <Empty title="还没有内容" sub={d.status === "pending" ? "正在整理，稍后刷新。" : "这篇文档没有可检索的文字。"} />
  return (
    <section ref={box} aria-label="片段"><SectionLabel>片段 · {d.chunks}{d.preview.length < d.chunks ? `（显示前 ${d.preview.length} 段）` : ""}</SectionLabel>
      <div className="flex flex-col gap-2">{d.preview.map((c) => (
        <div key={c.seq} data-seq={c.seq} aria-current={c.seq === seq ? "true" : undefined} className={cn("scroll-mt-24 rounded-xl border px-4 py-3", c.seq === seq && "border-blue/50 bg-blue/5 ring-2 ring-blue/20")}>
          {c.heading && <div className="mb-1 type-footnote font-medium text-muted-foreground">{c.heading}</div>}
          <div className="min-w-0 break-words" data-chunk-text><Markdown text={c.text} className="type-callout" /></div>
        </div>))}</div>
    </section>
  )
}

export function DocView({ id }: { id: string }) {
  const { d, setD, err } = useDoc(id)
  /* notes have their own editor page */
  const isNote = d?.source.kind === "notes"
  useEffect(() => { if (isNote) location.replace("#/knowledge/note/" + id + (location.hash.includes("?") ? "?" + location.hash.split("?")[1] : "")) }, [isNote, id])
  if (err) return <Page title="文档" back={BACK}><Empty title="打不开这篇文档" sub={err} /></Page>
  if (!d) return <Page title="文档" back={BACK}><Spin /></Page>
  if (isNote) return null
  const removable = d.source.kind === "files"
  return (
    <Page title={d.title} back={BACK} max={760} sub={<Meta><span>{d.source.name}</span>{d.ref && <span className="font-mono text-[12px]">{d.ref}</span>}</Meta>} actions={removable ? <DeleteButton d={d} what="文件" /> : undefined}>
      <div className="flex flex-col gap-8"><Props d={d} setD={setD} /><Chunks d={d} /></div>
    </Page>
  )
}
const Spin = () => <div className="flex justify-center py-16" role="status" aria-label="正在加载"><Spinner className="size-5 text-muted-foreground" /></div>

export function NoteView({ id }: { id: string }) {
  const { d, setD, err } = useDoc(id)
  const [title, setTitle] = useState(""), [body, setBody] = useState(""), [mode, setMode] = useState<"编辑" | "预览" | "片段">(seqOf() !== null ? "片段" : "编辑"), [busy, setBusy] = useState(false)
  useEffect(() => { if (d) { setTitle(d.title); setBody(d.body ?? "") } }, [d?.id]) // eslint-disable-line react-hooks/exhaustive-deps
  const dirty = !!d && (title.trim() !== d.title || body !== (d.body ?? ""))
  const save = async () => {
    if (!d || !dirty || busy || !title.trim()) return
    setBusy(true)
    try { setD(await kb.saveNote(d.id, { title: title.trim(), body })); toast("已保存") } catch (e) { toast(errMsg(e)) } finally { setBusy(false) }
  }
  const saveRef = useRef(save); saveRef.current = save
  useEffect(() => {
    const h = (e: KeyboardEvent) => { if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") { e.preventDefault(); void saveRef.current() } }
    window.addEventListener("keydown", h); return () => window.removeEventListener("keydown", h)
  }, [])
  if (err) return <Page title="笔记" back={BACK}><Empty title="打不开这篇笔记" sub={err} /></Page>
  if (!d) return <Page title="笔记" back={BACK}><Spin /></Page>
  return (
    <Page title={d.title} back={BACK} max={760} sub="笔记"
      actions={<><DeleteButton d={d} what="笔记" /><Button size="sm" disabled={!dirty || busy || !title.trim()} onClick={() => void save()} title="⌘S" data-note-save>{busy && <Spinner className="size-3.5" />}{dirty ? "保存" : "已保存"}</Button></>}>
      <div className="flex flex-col gap-8">
        <Props d={d} setD={setD} />
        <section className="flex flex-col gap-3" aria-label="正文">
          <div className="flex flex-wrap items-center justify-between gap-2"><Segmented size="sm" value={mode} onChange={setMode} options={["编辑", "预览", "片段"] as const} /><span className="type-footnote text-muted-foreground">⌘S 保存</span></div>
          {mode === "编辑" && <>
            <Input aria-label="标题" maxLength={120} value={title} onChange={(e) => setTitle(e.target.value)} className="h-10 text-[15px] font-medium" data-note-title />
            <Textarea aria-label="正文" value={body} onChange={(e) => setBody(e.target.value)} className="min-h-[360px] font-mono text-[13px] leading-[20px]" placeholder="支持 Markdown" data-note-body />
          </>}
          {mode === "预览" && <div className="min-h-40 rounded-xl border px-4 py-3 type-body">{body.trim() ? <Markdown text={body} /> : <span className="text-muted-foreground">还没有正文</span>}</div>}
          {mode === "片段" && <Chunks d={d} />}
        </section>
      </div>
    </Page>
  )
}
