import { api, outRawUrl, type RAnchor, type RAnnotation } from "@/api"
import { Button } from "@/components/ui/button"
import { Textarea } from "@/components/ui/textarea"
import { loadItem, tm, toast, type Out } from "@/store"
import { cn } from "cn"
import { CrosshairIcon, FileTextIcon, MessageSquarePlusIcon, PencilIcon, SendIcon, SquareDashedIcon, TextSelectIcon, Trash2Icon, XIcon } from "lucide-react"
import { useEffect, useRef, useState, type PointerEvent as PE, type RefObject } from "react"

/* notes on an output (ST-38): a region of a picture, selected text, a PDF page or the whole file. A note is a draft on the
 * Hub (so it survives a reload) until the user sends the item's drafts; they then reach the item as one message of the
 * user's, with each output's version and the places marked. One send carries one key: a second click, or a retry after a
 * lost answer, never sends twice. */

const pct = (f: number) => `${Math.round(f * 100)}%`
/** where on the output a note is, in words */
export function placeOf(a: { anchor: RAnchor; path?: string }, withFile = true) {
  const file = withFile && a.path ? a.path.split("/").pop() + " · " : "", at = a.anchor.at ? a.anchor.at + " · " : ""
  const x = a.anchor
  const what = x.type === "region" ? `区域（左 ${pct(x.x)}，上 ${pct(x.y)}，宽 ${pct(x.w)}，高 ${pct(x.h)}）`
    : x.type === "text" ? `「${x.quote.replace(/\s+/g, " ").slice(0, 60)}${x.quote.length > 60 ? "…" : ""}」${x.line ? `（第 ${x.line} 行）` : ""}`
    : x.type === "page" ? `第 ${x.page} 页` : "整体"
  return file + at + what
}

/* ── one send per key, kept per item until it went out ── */
const sendKeys = new Map<string, { ids: string; key: string }>()
const newKey = () => (crypto.randomUUID?.() ?? `${Date.now()}-${Math.random().toString(36).slice(2)}`)
const sending = new Set<string>()
export async function sendDrafts(item: string, drafts: RAnnotation[]): Promise<boolean> {
  if (!drafts.length || sending.has(item)) return false
  const ids = drafts.map((a) => a.id).sort().join(",")
  /* the same drafts keep their key across retries, so a send whose answer was lost is not sent again */
  const k = sendKeys.get(item)?.ids === ids ? sendKeys.get(item)!.key : newKey()
  sendKeys.set(item, { ids, key: k })
  sending.add(item)
  try {
    const r = await api.annSend(item, drafts.map((a) => a.id), k)
    sendKeys.delete(item)
    toast(r.again ? (r.pending ? "上一次发送还在进行，稍后刷新看看" : "这些批注已经发送过") : r.warning ? `${r.sent} 条批注已记到 ${item}，但没能送达会话：${r.warning}` : `${r.sent} 条批注已发送到 ${item}`)
    return true
  } catch (e) {
    toast(`没有发送成功，草稿还在：${(e as Error).message}`)
    return false
  } finally { sending.delete(item); void loadItem(item, true) }
}

/* ── selected text inside the shown output ── */
export type Sel = { anchor: Extract<RAnchor, { type: "text" }> }
/** the text selected inside `box`, kept until another selection replaces it or it is used; `source` is the file's text,
 * to find the line it is on */
export function useSelection(box: RefObject<HTMLElement | null>, source?: string) {
  const [sel, setSel] = useState<Sel | null>(null)
  useEffect(() => {
    const on = () => {
      const s = document.getSelection(), el = box.current
      if (!s || s.isCollapsed || !s.rangeCount || !el) return
      const r = s.getRangeAt(0)
      if (!el.contains(r.commonAncestorContainer)) return
      const quote = s.toString().trim()
      if (!quote) return
      const before = r.startContainer.textContent?.slice(0, r.startOffset) ?? "", after = r.endContainer.textContent?.slice(r.endOffset) ?? ""
      const host = (r.commonAncestorContainer instanceof Element ? r.commonAncestorContainer : r.commonAncestorContainer.parentElement)?.closest("[data-ann-at]")
      const at = host?.getAttribute("data-ann-at") || undefined
      let line: number | undefined
      if (source) { const i = source.indexOf(quote); if (i >= 0 && source.indexOf(quote, i + 1) < 0) line = source.slice(0, i).split("\n").length }
      setSel({ anchor: { type: "text", quote: quote.slice(0, 2000), prefix: before.slice(-32) || undefined, suffix: after.slice(0, 32) || undefined, line, at } })
    }
    document.addEventListener("selectionchange", on)
    return () => document.removeEventListener("selectionchange", on)
  }, [box, source])
  return [sel, setSel] as const
}

/** finds `quote` in the text under `box` (whitespace ignored) and marks it; scrolls it into view. False when absent. */
export function markText(box: HTMLElement, quote: string): boolean {
  const want = quote.replace(/\s+/g, "")
  if (!want) return false
  const pos: [Text, number][] = []; let flat = ""
  const w = document.createTreeWalker(box, NodeFilter.SHOW_TEXT)
  for (let n = w.nextNode() as Text | null; n; n = w.nextNode() as Text | null) {
    const t = n.data
    for (let i = 0; i < t.length; i++) if (!/\s/.test(t[i])) { flat += t[i]; pos.push([n, i]) }
  }
  const i = flat.indexOf(want)
  if (i < 0) return false
  const [sn, so] = pos[i], [en, eo] = pos[i + want.length - 1]
  const r = document.createRange(); r.setStart(sn, so); r.setEnd(en, eo + 1)
  const H = (globalThis as any).Highlight, reg = (CSS as any).highlights
  if (H && reg) reg.set("steve-ann", new H(r))
  else { const s = document.getSelection(); s?.removeAllRanges(); s?.addRange(r) }
  sn.parentElement?.scrollIntoView({ block: "center", behavior: "smooth" })
  return true
}
export const clearMark = () => (CSS as any).highlights?.delete?.("steve-ann")

/* ── a picture that takes regions ── */
type Box = { x: number; y: number; w: number; h: number }
const clamp = (v: number) => Math.min(1, Math.max(0, v))
/** `drawing`: dragging marks a region (`onDraw`) instead of scrolling or swiping; the notes on it are shown numbered */
export function AnnImage({ src, alt, notes, drawing, onDraw, focus, className, imgClass }: { src: string; alt: string; notes: { a: RAnnotation; n: number }[]; drawing: boolean; onDraw: (b: Box) => void; focus?: string; className?: string; imgClass?: string }) {
  const img = useRef<HTMLImageElement>(null)
  const [drag, setDrag] = useState<{ x0: number; y0: number; b: Box } | null>(null)
  const at = (e: PE) => { const r = img.current!.getBoundingClientRect(); return { x: clamp((e.clientX - r.left) / r.width), y: clamp((e.clientY - r.top) / r.height) } }
  useEffect(() => { if (focus && notes.some((x) => x.a.id === focus)) img.current?.parentElement?.scrollIntoView({ block: "center" }) }, [focus, notes])
  return (
    <div className={cn("relative inline-block max-w-full select-none", drawing && "cursor-crosshair touch-none", className)} data-ann-image
      onPointerDown={drawing ? (e) => { if (e.button !== 0) return; e.preventDefault(); (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId); const p = at(e); setDrag({ x0: p.x, y0: p.y, b: { ...p, w: 0, h: 0 } }) } : undefined}
      onPointerMove={drag ? (e) => { const p = at(e); setDrag({ ...drag, b: { x: Math.min(drag.x0, p.x), y: Math.min(drag.y0, p.y), w: Math.abs(p.x - drag.x0), h: Math.abs(p.y - drag.y0) } }) } : undefined}
      onPointerUp={drag ? () => { const b = drag.b; setDrag(null); if (b.w > 0.01 && b.h > 0.01) onDraw(b) } : undefined}
      onPointerCancel={() => setDrag(null)}>
      <img ref={img} src={src} alt={alt} draggable={false} className={cn("block max-w-full rounded-lg border bg-muted object-contain", imgClass)} data-out-image />
      {notes.map(({ a, n }) => a.anchor.type === "region" && (
        <div key={a.id} className={cn("pointer-events-none absolute rounded-[3px] border-2", a.status === "sent" ? "border-blue bg-blue/10" : "border-attention bg-attention/10", focus === a.id && "ring-4 ring-blue/40")}
          style={{ left: pct(a.anchor.x), top: pct(a.anchor.y), width: pct(a.anchor.w), height: pct(a.anchor.h) }} data-ann-box={a.status}>
          <span className={cn("absolute -left-2 -top-2 flex size-4.5 items-center justify-center rounded-full text-[10px] font-semibold text-white", a.status === "sent" ? "bg-blue" : "bg-attention")}>{n}</span>
        </div>))}
      {drag && <div className="pointer-events-none absolute border-2 border-dashed border-attention bg-attention/10" style={{ left: pct(drag.b.x), top: pct(drag.b.y), width: pct(drag.b.w), height: pct(drag.b.h) }} />}
    </div>
  )
}

/** the region of a picture a note marks, as a small crop */
export function RegionThumb({ src, b, className }: { src: string; b: Box; className?: string }) {
  const [ratio, setRatio] = useState(0)
  const W = 72, iw = W / b.w, ih = ratio ? iw * ratio : 0, H = ih ? Math.min(72, Math.max(24, b.h * ih)) : 48
  return (
    <span className={cn("relative block shrink-0 overflow-hidden rounded-md border bg-muted", className)} style={{ width: W, height: H }} data-ann-thumb>
      <img src={src} alt="" loading="lazy" onLoad={(e) => setRatio(e.currentTarget.naturalHeight / Math.max(1, e.currentTarget.naturalWidth))}
        className="absolute max-w-none" style={{ width: iw, left: -b.x * iw, top: ih ? -b.y * ih + (H - b.h * ih) / 2 : 0, opacity: ih ? 1 : 0 }} />
    </span>
  )
}

/* ── the notes bar under a shown output ── */
export type Pending = { art: string; path: string; anchor: RAnchor }
export function AnnBar({ item, art, path, notes, allDrafts, pending, setPending, sel, clearSel, canDraw, drawing, setDrawing, pdf, onLocate }: {
  item: string; /** the output shown and the file in it, for notes on the whole of it, a page or selected text */ art: string; path: string; notes: { a: RAnnotation; n: number }[]; allDrafts: RAnnotation[]
  pending: Pending | null; setPending: (p: Pending | null) => void; sel: Sel | null; clearSel: () => void
  canDraw: boolean; drawing: boolean; setDrawing: (v: boolean) => void; pdf: boolean; onLocate: (a: RAnnotation) => void
}) {
  const [text, setText] = useState(""), [page, setPage] = useState("1"), [busy, setBusy] = useState(false)
  const [edit, setEdit] = useState<{ id: string; text: string } | null>(null)
  const [sendBusy, setSendBusy] = useState(false)
  useEffect(() => { setText("") }, [pending])
  const drafts = notes.filter((x) => x.a.status !== "sent"), sent = notes.filter((x) => x.a.status === "sent")
  const elsewhere = allDrafts.filter((a) => !notes.some((x) => x.a.id === a.id)).length
  const save = async () => {
    if (!pending || !text.trim() || busy) return
    const anchor: RAnchor = pending.anchor.type === "page" ? { ...pending.anchor, page: Math.max(1, Math.floor(+page) || 1) } : pending.anchor
    setBusy(true)
    try { await api.annAdd(item, { art: pending.art, path: pending.path || undefined, anchor, note: text.trim() }); setPending(null); setDrawing(false); toast("已存为草稿，确认发送前可以修改"); void loadItem(item, true) }
    catch (e) { toast(`没有存上：${(e as Error).message}`) } finally { setBusy(false) }
  }
  const act = async (f: () => Promise<unknown>, ok: string) => { try { await f(); toast(ok) } catch (e) { toast((e as Error).message) } finally { void loadItem(item, true) } }
  return (
    <div className="flex flex-col gap-2 border-t pt-3" data-ann-bar>
      <div className="flex flex-wrap items-center gap-1.5">
        <span className="mr-auto flex items-center gap-1.5 type-callout"><MessageSquarePlusIcon className="size-4 text-muted-foreground" />批注
          {(drafts.length > 0 || sent.length > 0) && <span className="type-footnote text-muted-foreground">{[drafts.length && `草稿 ${drafts.length}`, sent.length && `已发送 ${sent.length}`].filter(Boolean).join(" · ")}</span>}</span>
        {canDraw && <Button size="sm" variant={drawing ? "secondary" : "ghost"} aria-pressed={drawing} onClick={() => { setDrawing(!drawing); setPending(null) }} data-ann-draw><SquareDashedIcon />{drawing ? "框选中，拖动画出区域" : "框选区域"}</Button>}
        {pdf && <Button size="sm" variant="ghost" onClick={() => setPending({ art, path, anchor: { type: "page", page: 1 } })} data-ann-page><FileTextIcon />批注某页</Button>}
        <Button size="sm" variant="ghost" onClick={() => setPending({ art, path, anchor: { type: "file" } })} data-ann-file><PencilIcon />整体批注</Button>
      </div>
      {sel && !pending && <div className="flex items-center gap-2 rounded-lg bg-muted/60 px-2.5 py-1.5" data-ann-sel>
        <TextSelectIcon className="size-4 shrink-0 text-muted-foreground" /><span className="min-w-0 flex-1 truncate type-footnote">已选中「{sel.anchor.quote.replace(/\s+/g, " ")}」</span>
        <Button size="sm" onClick={() => { setPending({ art, path, anchor: sel.anchor }); clearSel() }} data-ann-sel-add>批注所选</Button>
        <Button size="icon-sm" variant="ghost" aria-label="取消选中" onClick={clearSel}><XIcon /></Button>
      </div>}
      {pending && <div className="flex flex-col gap-2 rounded-xl border p-2.5" data-ann-new={pending.anchor.type}>
        <div className="flex items-center gap-2 type-footnote text-muted-foreground"><CrosshairIcon className="size-3.5 shrink-0" />
          {pending.anchor.type === "page" ? <label className="flex items-center gap-1.5">第<input type="number" min={1} value={page} onChange={(e) => setPage(e.target.value)} className="h-6 w-16 rounded-md border bg-background px-1.5 text-foreground" data-ann-page-input />页</label>
            : <span className="min-w-0 truncate">{placeOf({ anchor: pending.anchor, path: pending.path })}</span>}</div>
        <Textarea autoFocus value={text} onChange={(e) => setText(e.target.value)} placeholder="写下要改什么，或者有什么问题" className="min-h-16 rounded-lg" data-ann-input
          onKeyDown={(e) => { e.stopPropagation(); if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) void save() }} />
        <div className="flex justify-end gap-2"><Button size="sm" variant="ghost" onClick={() => setPending(null)}>取消</Button><Button size="sm" disabled={!text.trim() || busy} onClick={() => void save()} data-ann-save>存为草稿</Button></div>
      </div>}
      {notes.length > 0 && <ol className="flex max-h-48 flex-col overflow-auto" data-ann-list>
        {notes.map(({ a, n }) => (
          <li key={a.id} className="group/ann flex items-start gap-2 py-1.5" data-ann={a.status}>
            <span className={cn("mt-0.5 flex size-4.5 shrink-0 items-center justify-center rounded-full text-[10px] font-semibold text-white", a.status === "sent" ? "bg-blue" : "bg-attention")}>{n}</span>
            <div className="min-w-0 flex-1">
              <button type="button" className="block max-w-full truncate text-left type-footnote text-muted-foreground hover:text-foreground" onClick={() => onLocate(a)} data-ann-locate>{placeOf(a)}</button>
              {edit?.id === a.id ? <div className="mt-1 flex flex-col gap-1.5"><Textarea autoFocus value={edit.text} onChange={(e) => setEdit({ ...edit, text: e.target.value })} onKeyDown={(e) => e.stopPropagation()} className="min-h-14 rounded-lg" data-ann-edit-input />
                <div className="flex justify-end gap-2"><Button size="sm" variant="ghost" onClick={() => setEdit(null)}>取消</Button><Button size="sm" disabled={!edit.text.trim()} onClick={() => { const t = edit.text.trim(); setEdit(null); void act(() => api.annEdit(item, a.id, t), "草稿已更新") }} data-ann-edit-save>保存</Button></div></div>
                : <div className="type-callout whitespace-pre-wrap break-words">{a.note}</div>}
              {a.status !== "draft" && <div className="type-footnote text-tertiary">{a.status === "sent" ? `已发送 · ${tm(a.sent ?? a.updated)}` : "发送中…"}</div>}
            </div>
            {a.status === "draft" && edit?.id !== a.id && <div className="flex shrink-0 gap-0.5 opacity-70 group-hover/ann:opacity-100">
              <Button size="icon-sm" variant="ghost" aria-label="修改" onClick={() => setEdit({ id: a.id, text: a.note })} data-ann-edit><PencilIcon /></Button>
              <Button size="icon-sm" variant="ghost" aria-label="删除" onClick={() => void act(() => api.annRemove(item, a.id), "草稿已删除")} data-ann-del><Trash2Icon /></Button>
            </div>}
          </li>))}
      </ol>}
      {allDrafts.length > 0 && <div className="flex flex-wrap items-center justify-end gap-2" data-ann-send-row>
        <span className="mr-auto type-footnote text-muted-foreground">确认后作为你的回复发到 {item}{elsewhere > 0 && `，含其他产出上的 ${elsewhere} 条`}</span>
        <Button size="sm" disabled={sendBusy} onClick={() => { setSendBusy(true); void sendDrafts(item, allDrafts).finally(() => setSendBusy(false)) }} data-ann-send><SendIcon />{sendBusy ? "发送中…" : `确认发送 ${allDrafts.length} 条`}</Button>
      </div>}
    </div>
  )
}

/** a sent note in the item's activity: what it marks, on which version of which output; opens the output there */
export function AnnRefs({ item, anns, outs, open }: { item: string; anns: RAnnotation[]; outs: Out[]; open: (o: Out, ann: string) => void }) {
  return (
    <div className="mt-1 flex flex-col gap-1.5" data-ann-refs>
      {anns.map((a, n) => {
        const o = outs.find((x) => x[3] === a.artifact_id)
        const same = o ? outs.filter((x) => x[0] === o[0] && (x[2] || x[1]) === (o[2] || o[1])) : []
        const v = o ? same.indexOf(o) + 1 : 0
        return (
          <button key={a.id} type="button" disabled={!o} onClick={() => o && open(o, a.id)} className="flex items-start gap-2.5 rounded-lg px-1 py-1 text-left hover:bg-muted/60 disabled:hover:bg-transparent" data-ann-ref={a.id}>
            {a.anchor.type === "region" ? <RegionThumb src={outRawUrl(item, a.artifact_id, a.path || undefined)} b={a.anchor} />
              : <span className="flex size-12 shrink-0 items-center justify-center rounded-md border bg-muted text-muted-foreground">{a.anchor.type === "text" ? <TextSelectIcon className="size-4" /> : <FileTextIcon className="size-4" />}</span>}
            <span className="min-w-0 flex-1">
              <span className="block truncate type-footnote text-muted-foreground">{n + 1}. {o ? o[1] : "产出已不在"}{v > 0 && ` · 第 ${v} 版`} · {placeOf(a)}</span>
              <span className="block type-callout whitespace-pre-wrap break-words">{a.note}</span>
            </span>
            {o && <span className="shrink-0 self-center type-footnote text-blue">定位</span>}
          </button>
        )
      })}
    </div>
  )
}
