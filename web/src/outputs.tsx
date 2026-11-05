import { api, outRawUrl, type RAnnotation, type ROutPreview } from "@/api"
import { AnnBar, AnnImage, clearMark, markText, useSelection, type Pending } from "@/annotate"
import { size } from "@/attach"
import { Markdown } from "@/components/markdown"
import { Button } from "@/components/ui/button"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { Spinner } from "@/components/ui/spinner"
import { tm, toast, type Out } from "@/store"
import { Segmented } from "@/ui"
import { cn } from "cn"
import { OfficeView } from "@/office"
import { ChevronLeftIcon, ChevronRightIcon, CopyIcon, DownloadIcon, ExternalLinkIcon, FileIcon, FileTextIcon, FolderIcon, GalleryVerticalIcon, GitBranchIcon, ZoomInIcon, ZoomOutIcon } from "lucide-react"
import { useEffect, useRef, useState, type ReactNode } from "react"
import { useT } from "@/i18n"

/* what an item's output holds, shown as itself: documents rendered, code highlighted, tables as tables, images,
 * PDFs and media in place, folders browsable, branches and commits as their commits and diff */

/** above this, code is shown plain: highlighting half a megabyte freezes the page */
const HIGHLIGHT_MAX = 120_000
const fence = (lang: string, t: string) => { const n = Math.max(3, ...[...t.matchAll(/`{3,}/g)].map((m) => m[0].length + 1)); const f = "`".repeat(n); return `${f}${lang}\n${t}\n${f}` }

function Note({ children }: { children: ReactNode }) {
  return <div className="rounded-lg bg-muted/60 px-3 py-2 type-footnote text-muted-foreground">{children}</div>
}
function Missing({ reason }: { reason: string }) {
  const t = useT()
  return <div className="flex flex-col items-center gap-1 py-12 text-center" data-out-missing><FileIcon className="size-6 text-tertiary" /><div className="type-callout">{t("pages:outputs.cantView")}</div><div className="max-w-md type-footnote text-muted-foreground">{reason}</div></div>
}
function Plain({ text }: { text: string }) {
  return <pre className="max-h-[60vh] overflow-auto rounded-xl bg-muted/60 p-3 font-mono text-[12.5px] leading-5 whitespace-pre-wrap break-words" data-out-plain>{text}</pre>
}
function Code({ lang, text }: { lang: string; text: string }) {
  return text.length > HIGHLIGHT_MAX || lang === "text" ? <Plain text={text} /> : <Markdown className="type-callout [&_[data-streamdown=code-block]]:my-0" text={fence(lang, text)} />
}

/* CSV with quoted fields; the first row is the header */
function parseCsv(t: string, sep: string): string[][] {
  const rows: string[][] = []; let row: string[] = [], cell = "", q = false
  for (let i = 0; i < t.length; i++) {
    const c = t[i]
    if (q) { if (c === '"' && t[i + 1] === '"') { cell += '"'; i++ } else if (c === '"') q = false; else cell += c; continue }
    if (c === '"') q = true
    else if (c === sep) { row.push(cell); cell = "" }
    else if (c === "\n" || c === "\r") { if (c === "\r" && t[i + 1] === "\n") i++; row.push(cell); rows.push(row); row = []; cell = ""; if (rows.length > 500) break }
    else cell += c
  }
  if (cell || row.length) { row.push(cell); rows.push(row) }
  return rows
}
function Table({ text, sep }: { text: string; sep: string }) {
  const rows = parseCsv(text, sep), [head, ...body] = rows
  if (!head) return <Plain text={text} />
  return (
    <div className="max-h-[60vh] overflow-auto rounded-xl border" data-out-table>
      <table className="w-full border-collapse type-footnote">
        <thead className="sticky top-0 bg-muted"><tr>{head.map((h, n) => <th key={n} className="border-b px-2.5 py-1.5 text-left font-medium whitespace-nowrap">{h}</th>)}</tr></thead>
        <tbody>{body.slice(0, 500).map((r, n) => <tr key={n} className="border-b last:border-0">{head.map((_, k) => <td key={k} className="px-2.5 py-1 align-top">{r[k] ?? ""}</td>)}</tr>)}</tbody>
      </table>
    </div>
  )
}
function Html({ text }: { text: string }) {
  const t = useT()
  return <iframe title={t("pages:outputs.htmlTitle")} sandbox="allow-scripts" referrerPolicy="no-referrer" srcDoc={text} className="block h-[60vh] w-full rounded-xl border bg-white" data-out-html />
}

/** a text file by what it is: a document, a page, a table, a diagram, JSON or code; rendered ones can show their source */
function TextView({ p }: { p: Extract<ROutPreview, { type: "text" }> }) {
  const t = useT()
  const rendered = ["markdown", "html", "csv", "tsv", "mermaid"].includes(p.lang)
  const [tab, setTab] = useState<"预览" | "源码">("预览")
  let json = p.text
  if (p.lang === "json" && !p.truncated) try { json = JSON.stringify(JSON.parse(p.text), null, 2) } catch { /* JSON lines, or broken: as written */ }
  const body = !rendered || tab === "源码" ? <Code lang={p.lang} text={p.lang === "json" ? json : p.text} />
    : p.lang === "markdown" ? <div className="max-h-[60vh] overflow-auto rounded-xl border px-5 py-4" data-out-md><Markdown className="type-body" text={p.text} /></div>
    : p.lang === "html" ? <Html text={p.text} />
    : p.lang === "mermaid" ? <Markdown text={fence("mermaid", p.text)} />
    : <Table text={p.text} sep={p.lang === "tsv" ? "\t" : ","} />
  return (
    <div className="flex flex-col gap-2" data-out-text={p.lang}>
      {rendered && <Segmented size="sm" value={tab} onChange={setTab} options={[["预览", t("pages:outputs.preview")], ["源码", t("pages:outputs.source")]] as const} />}
      {p.from && <Note>{t("pages:outputs.from", { from: p.from })}</Note>}
      {body}
      {p.truncated && <Note>{t("pages:outputs.truncText")}</Note>}
    </div>
  )
}

/** a unified diff, one collapsible block per file */
function Diff({ diff, truncated }: { diff: string; truncated: boolean }) {
  const t = useT()
  const files = diff ? diff.split(/^(?=diff --git )/m).filter(Boolean) : []
  if (!files.length) return null
  return (
    <div className="flex flex-col gap-1.5" data-out-diff>
      {files.map((f, n) => {
        const name = /^diff --git a\/(.+?) b\//.exec(f)?.[1] ?? "?"
        const lines = f.replace(/\n+$/, "").split("\n"), body = lines.slice(lines.findIndex((l) => l.startsWith("@@")))
        const add = lines.filter((l) => l.startsWith("+") && !l.startsWith("+++")).length, del = lines.filter((l) => l.startsWith("-") && !l.startsWith("---")).length
        return (
          <Collapsible key={n} defaultOpen={files.length <= 4} className="overflow-hidden rounded-lg border">
            <CollapsibleTrigger className="group/f flex w-full items-center gap-2 bg-muted/60 px-2.5 py-1.5 text-left hover:bg-muted">
              <ChevronRightIcon className="size-3.5 shrink-0 text-muted-foreground transition-transform group-data-[panel-open]/f:rotate-90" />
              <span className="min-w-0 flex-1 truncate font-mono text-[12.5px]">{name}</span>
              <span className="shrink-0 font-mono text-[12px] tabular-nums"><span className="text-success">+{add}</span> <span className="text-destructive">−{del}</span></span>
            </CollapsibleTrigger>
            <CollapsibleContent>
              <pre className="max-h-[50vh] overflow-auto py-1 font-mono text-[12px] leading-[18px]">{body[0]?.startsWith("@@") ? body.map((l, k) => (
                <div key={k} className={cn("px-3 whitespace-pre", l.startsWith("@@") ? "bg-blue/10 text-muted-foreground" : l.startsWith("+") ? "bg-success/8" : l.startsWith("-") ? "bg-destructive/8" : "")}>{l || " "}</div>
              )) : <div className="px-3 text-muted-foreground">{t("pages:outputs.binaryDiff")}</div>}</pre>
            </CollapsibleContent>
          </Collapsible>
        )
      })}
      {truncated && <Note>{t("pages:outputs.truncDiff")}</Note>}
    </div>
  )
}
const statLast = (stat: string) => stat.trim().split("\n").pop() ?? ""

/** `base`: the path walked below the output so far; an entry opens as `base/name` */
function Folder({ p, item, art, base, open, browse }: { p: Extract<ROutPreview, { type: "dir" }>; item: string; art: string; base: string; open: (sub: string) => void; browse: () => void }) {
  const t = useT()
  const at = (n: string) => (base ? base + "/" : "") + n
  const pics = p.entries.filter((e) => e.form === "image"), rest = p.entries.filter((e) => e.form !== "image")
  if (!p.entries.length) return <Missing reason={t("pages:outputs.emptyDir")} />
  return (
    <div className="flex flex-col gap-3" data-out-dir>
      {pics.length > 1 && <Button size="sm" variant="outline" className="self-start" onClick={browse} data-out-browse><GalleryVerticalIcon />{t("pages:outputs.browseAll", { count: pics.length })}</Button>}
      {pics.length > 0 && <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">{pics.map((e) => (
        <button key={e.name} type="button" onClick={() => open(at(e.name))} className="group flex flex-col gap-1 text-left" data-out-entry={e.name}>
          <img src={outRawUrl(item, art, at(e.name))} alt={e.name} loading="lazy" className="aspect-[4/3] w-full rounded-lg border bg-muted object-cover object-top transition-opacity group-hover:opacity-85" />
          <span className="truncate type-footnote text-muted-foreground">{e.name}</span>
        </button>))}</div>}
      {rest.length > 0 && <div className="flex flex-col overflow-hidden rounded-xl border">{rest.map((e) => (
        <button key={e.name} type="button" onClick={() => open(at(e.name))} className="flex items-center gap-2.5 border-b px-3 py-2 text-left last:border-0 hover:bg-muted/60" data-out-entry={e.name}>
          {e.dir ? <FolderIcon className="size-4 text-blue" /> : <FileTextIcon className="size-4 text-muted-foreground" />}
          <span className="min-w-0 flex-1 truncate type-callout">{e.name}</span>
          {e.size != null && <span className="shrink-0 type-footnote tabular-nums text-muted-foreground">{size(e.size)}</span>}
          <ChevronRightIcon className="size-4 shrink-0 text-tertiary" />
        </button>))}</div>}
      {p.truncated && <Note>{t("pages:outputs.truncDir")}</Note>}
    </div>
  )
}
function Media({ p, url, page }: { p: Extract<ROutPreview, { type: "image" | "pdf" | "video" | "audio" }>; url: string; page?: number }) {
  const t = useT()
  if (p.large) return <Missing reason={t("pages:outputs.tooLarge", { size: size(p.size ?? 0) })} />
  return p.type === "pdf" ? <iframe key={page ?? 0} title={p.name} src={page ? `${url}#page=${page}` : url} className="block h-[65vh] w-full rounded-xl border" data-out-pdf />
    : p.type === "video" ? <video src={url} controls className="max-h-[65vh] w-full rounded-xl bg-black" data-out-video />
    : <audio src={url} controls className="w-full" data-out-audio />
}

/** one picture of a set: where it is in the output, and how to show and fetch it */
type Pic = { key: string; name: string; art: string; path: string; src: string; out?: Out }
/** pictures one after another: a swipe, the arrow keys or the buttons move between them; the strip below jumps.
 * 连续 shows them all, one under another. While regions are drawn or the picture is zoomed, dragging belongs to the picture. */
function Gallery({ pics, at, go, notesOf, drawing, onDraw, focus, mode, setMode }: { pics: Pic[]; at: number; go: (n: number) => void; notesOf: (p: Pic) => { a: RAnnotation; n: number }[]
  drawing: boolean; onDraw: (p: Pic, b: { x: number; y: number; w: number; h: number }) => void; focus?: string; mode: "逐张" | "连续"; setMode: (m: "逐张" | "连续") => void }) {
  const t = useT()
  const [zoom, setZoom] = useState(false)
  const start = useRef<{ x: number; y: number } | null>(null), strip = useRef<HTMLDivElement>(null)
  const cur = pics[at], many = pics.length > 1
  useEffect(() => { setZoom(false) }, [at])
  useEffect(() => { strip.current?.querySelector<HTMLElement>(`[data-pic-thumb="${at}"]`)?.scrollIntoView({ block: "nearest", inline: "center" }) }, [at])
  useEffect(() => {
    if (!many || mode !== "逐张") return
    const k = (e: KeyboardEvent) => {
      if (e.target instanceof HTMLElement && e.target.closest("input, textarea, [contenteditable], [role=tablist]")) return
      if (e.key === "ArrowLeft" && at > 0) { e.preventDefault(); go(at - 1) }
      if (e.key === "ArrowRight" && at < pics.length - 1) { e.preventDefault(); go(at + 1) }
    }
    /* the dialog keeps its keys to itself: listen before it does */
    document.addEventListener("keydown", k, true)
    return () => document.removeEventListener("keydown", k, true)
  }, [many, mode, at, pics.length, go])
  if (!cur) return null
  const bar = (
    <div className="flex items-center gap-2" data-gallery-bar>
      {many && <Segmented size="sm" value={mode} onChange={setMode} options={[["逐张", t("pages:outputs.oneByOne")], ["连续", t("pages:outputs.all")]] as const} />}
      {many && mode === "逐张" && <span className="type-footnote tabular-nums text-muted-foreground" data-gallery-count>{at + 1} / {pics.length}</span>}
      <span className="min-w-0 flex-1 truncate type-footnote text-muted-foreground">{mode === "逐张" ? cur.name : t("pages:outputs.allHint", { count: pics.length })}</span>
      {mode === "逐张" && <Button size="sm" variant="ghost" aria-pressed={zoom} onClick={() => setZoom(!zoom)} data-gallery-zoom>{zoom ? <ZoomOutIcon /> : <ZoomInIcon />}{zoom ? t("pages:outputs.zoomReset") : t("pages:outputs.zoomIn")}</Button>}
    </div>
  )
  if (mode === "连续") return (
    <div className="flex flex-col gap-3" data-gallery="连续">
      {bar}
      <div className="flex flex-col gap-5" data-gallery-scroll>
        {pics.map((p, n) => <figure key={p.key} className="flex flex-col items-center gap-1.5" data-pic={n}>
          <AnnImage src={p.src} alt={p.name} notes={notesOf(p)} drawing={drawing} onDraw={(b) => onDraw(p, b)} focus={focus} />
          <figcaption className="type-footnote text-muted-foreground"><span className="tabular-nums">{n + 1} / {pics.length}</span> · {p.name}</figcaption>
        </figure>)}
      </div>
    </div>
  )
  const free = !drawing && !zoom
  return (
    <div className="flex min-w-0 flex-col gap-2" data-gallery="逐张">
      {bar}
      <div className={cn("relative flex min-h-40 justify-center rounded-xl bg-muted/40", zoom ? "max-h-[65vh] justify-start overflow-auto" : "items-center", free && "touch-pan-y")} data-gallery-stage
        onPointerDown={free ? (e) => { start.current = { x: e.clientX, y: e.clientY } } : undefined}
        onPointerUp={free ? (e) => {
          const s = start.current; start.current = null
          if (!s) return
          const dx = e.clientX - s.x, dy = e.clientY - s.y
          if (Math.abs(dx) > 50 && Math.abs(dx) > Math.abs(dy) * 1.5) { if (dx < 0 && at < pics.length - 1) go(at + 1); if (dx > 0 && at > 0) go(at - 1) }
        } : undefined}
        onDoubleClick={() => !drawing && setZoom(!zoom)}>
        <AnnImage key={cur.key} src={cur.src} alt={cur.name} notes={notesOf(cur)} drawing={drawing} onDraw={(b) => onDraw(cur, b)} focus={focus}
          className={cn(zoom && "max-w-none")} imgClass={zoom ? "max-w-none w-[200%]" : "max-h-[55vh] max-sm:max-h-[45vh]"} />
        {many && !zoom && <>
          <Button size="icon" variant="outline" aria-label={t("pages:outputs.prev")} disabled={at === 0} onClick={() => go(at - 1)} className="absolute left-2 top-1/2 -translate-y-1/2 rounded-full bg-background/85 backdrop-blur disabled:opacity-0" data-gallery-prev><ChevronLeftIcon /></Button>
          <Button size="icon" variant="outline" aria-label={t("pages:outputs.next")} disabled={at === pics.length - 1} onClick={() => go(at + 1)} className="absolute right-2 top-1/2 -translate-y-1/2 rounded-full bg-background/85 backdrop-blur disabled:opacity-0" data-gallery-next><ChevronRightIcon /></Button>
        </>}
      </div>
      {many && <div ref={strip} className="flex gap-1.5 overflow-x-auto pb-1" data-gallery-strip>
        {pics.map((p, n) => <button key={p.key} type="button" aria-label={t("pages:outputs.thumb", { n: n + 1, name: p.name })} aria-current={n === at} onClick={() => go(n)} data-pic-thumb={n}
          className={cn("h-12 w-16 shrink-0 overflow-hidden rounded-md border-2 transition-opacity", n === at ? "border-blue" : "border-transparent opacity-60 hover:opacity-100")}>
          <img src={p.src} alt="" loading="lazy" className="size-full object-cover object-top" /></button>)}
      </div>}
    </div>
  )
}

function Commits({ cs }: { cs: { hash: string; subject: string; author: string; at: number | null }[] }) {
  return (
    <div className="flex flex-col overflow-hidden rounded-xl border" data-out-commits>
      {cs.map((c) => <div key={c.hash} className="flex items-baseline gap-2.5 border-b px-3 py-1.5 last:border-0">
        <span className="shrink-0 font-mono text-[12px] text-muted-foreground">{c.hash}</span><span className="min-w-0 flex-1 truncate type-callout">{c.subject}</span>
        {c.at && <span className="shrink-0 type-footnote text-tertiary">{tm(c.at)}</span>}</div>)}
    </div>
  )
}

const IMG = /\.(png|jpe?g|gif|webp|avif|bmp|svg)$/i
/** how pictures were last looked at, kept while moving between outputs */
let lastMode: "逐张" | "连续" = "逐张"
type Entry = Extract<ROutPreview, { type: "dir" }>["entries"][number]

/** the body of the output dialog; `loc` and the kind come from the row, the rest from the Hub. `siblings`: the other
 * pictures delivered with this one, to move between; `focus`: a note to show where it is; `anns`: the item's notes */
export function OutPreview({ o, item, anns = [], focus: focus0, siblings = [], outs = [], onOpen }: { o: Out; item: string; anns?: RAnnotation[]; focus?: string; siblings?: Out[]; outs?: Out[]; onOpen?: (o: Out, focus?: string) => void }) {
  const t = useT()
  const [k, , loc, art] = o
  const start = anns.find((a) => a.id === focus0 && a.artifact_id === art)
  const [path, setPath] = useState<string[]>(start?.path ? start.path.split("/") : [])
  const [p, setP] = useState<ROutPreview | null>(null), [err, setErr] = useState("")
  const [dirs, setDirs] = useState<Record<string, Entry[]>>({})
  const [mode, setMode0] = useState(lastMode)
  const setMode = (m: "逐张" | "连续") => { lastMode = m; setMode0(m) }
  const [drawing, setDrawing] = useState(false), [pending, setPending] = useState<Pending | null>(null)
  const [focus, setFocus] = useState(focus0)
  const box = useRef<HTMLDivElement>(null)
  const [sel, setSel] = useSelection(box, p?.type === "text" ? p.text : undefined)
  const at = path.join("/"), parent = path.slice(0, -1).join("/")
  useEffect(() => {
    let live = true; setP(null); setErr(""); setSel(null)
    api.outPreview(item, art, at || undefined).then((v) => { if (!live) return; setP(v); if (v.type === "dir") setDirs((d) => ({ ...d, [at]: v.entries })) }, (e: Error) => live && setErr(e.message))
    return () => { live = false }
  }, [item, art, at, setSel])
  /* a picture inside a folder moves between the folder's pictures: the folder is listed once */
  const inFolder = p?.type === "image" && path.length > 0
  useEffect(() => {
    if (!inFolder || dirs[parent]) return
    let live = true
    api.outPreview(item, art, parent || undefined).then((v) => live && v.type === "dir" && setDirs((d) => ({ ...d, [parent]: v.entries })), () => {})
    return () => { live = false }
  }, [inFolder, parent, dirs, item, art])
  const pics: Pic[] = p?.type !== "image" ? []
    : inFolder ? (dirs[parent] ?? []).filter((e) => e.form === "image").map((e) => { const sub = parent ? `${parent}/${e.name}` : e.name; return { key: `${art}/${sub}`, name: e.name, art, path: sub, src: outRawUrl(item, art, sub) } })
    : [o, ...siblings.filter((x) => x[3] !== art)].filter((x) => x[3] === art || (x[2] && IMG.test(x[2].trim()))).sort((a, b) => siblings.findIndex((s) => s[3] === a[3]) - siblings.findIndex((s) => s[3] === b[3]))
      .map((x) => ({ key: x[3], name: x[1], art: x[3], path: "", src: outRawUrl(item, x[3]), out: x }))
  const cur = Math.max(0, pics.findIndex((x) => x.art === art && x.path === at))
  const goPic = (n: number) => { const x = pics[n]; if (!x) return; if (x.out && x.art !== art) onOpen?.(x.out); else setPath(x.path ? x.path.split("/") : []) }

  /* the notes on what is shown, numbered as they were written */
  const here = (a: RAnnotation) => mode === "连续" && pics.length > 1 ? pics.some((x) => x.art === a.artifact_id && x.path === a.path)
    : a.artifact_id === art && (p?.type === "dir" ? true : a.path === at)
  const notes = anns.filter(here).sort((a, b) => a.created - b.created).map((a, n) => ({ a, n: n + 1 }))
  const notesOf = (x: Pic) => notes.filter(({ a }) => a.artifact_id === x.art && a.path === x.path)
  /* a send the Hub was stopped in the middle of frees its drafts after five minutes */
  const drafts = anns.filter((a) => a.status === "draft" || (a.status === "sending" && Date.now() - a.updated > 300_000))
  const focused = anns.find((a) => a.id === focus)
  const page = focused?.artifact_id === art && focused.anchor.type === "page" ? focused.anchor.page : undefined
  /* show where the focused note is once its text has rendered (documents, sheets and slides render after loading) */
  useEffect(() => {
    clearMark()
    if (!p || !focused || focused.artifact_id !== art || focused.path !== at || focused.anchor.type !== "text") return
    const quote = focused.anchor.quote
    let n = 0, t = 0
    const tick = () => { if (box.current && markText(box.current, quote)) return; if (++n < 20) t = window.setTimeout(tick, 200) }
    tick()
    return () => { window.clearTimeout(t); clearMark() }
  }, [p, focused, art, at])
  const locate = (a: RAnnotation) => {
    setFocus(a.id)
    if (a.artifact_id !== art) { const x = outs.find((s) => s[3] === a.artifact_id); if (x) onOpen?.(x, a.id); return }
    if (a.path !== at && !(mode === "连续" && pics.length > 1)) setPath(a.path ? a.path.split("/") : [])
  }

  const media = !!p && ["image", "pdf", "video", "audio", "office"].includes(p.type)
  /* a text git still had (its worktree is gone) has no file to download */
  const file = media || p?.type === "binary" || (p?.type === "text" && !p.from)
  const raw = (dl = false) => outRawUrl(item, art, at || undefined, dl)
  const where = p && "path" in p ? p.path : loc
  /* what a branch or commit is known by, to paste into git */
  const ref = p?.type === "commit" ? p.hash : p?.type === "branch" ? p.branch : null
  const legacy = p?.type === "binary" && /\.(doc|xls|ppt|pages|numbers|key|odt|ods|odp)$/i.test(p.name)
  const body = err ? <Missing reason={err} />
    : !p ? <div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div>
    : p.type === "missing" ? <Missing reason={p.reason} />
    : p.type === "link" ? <div className="flex flex-col items-start gap-3 py-2"><span className="break-all font-mono text-[12.5px]">{p.url}</span><Button size="sm" onClick={() => window.open(p.url, "_blank", "noopener")}><ExternalLinkIcon />{t("pages:outputs.openLink")}</Button></div>
    : p.type === "text" ? <TextView key={at} p={p} />
    : p.type === "binary" ? <Missing reason={legacy ? t("pages:outputs.legacy", { name: p.name }) : t("pages:outputs.binary", { name: p.name, size: size(p.size ?? 0) })} />
    : p.type === "office" ? (p.large ? <Missing reason={t("pages:outputs.tooLarge", { size: size(p.size ?? 0) })} /> : <OfficeView url={raw()} name={p.name} fallback={(r) => <Missing reason={r} />} />)
    : p.type === "dir" ? <Folder p={p} item={item} art={art} base={at} open={(s) => setPath(s.split("/"))} browse={() => { const e = p.entries.find((x) => x.form === "image"); if (e) { setMode("连续"); setPath([...path, e.name]) } }} />
    : p.type === "commit" ? <div className="flex flex-col gap-3" data-out-commit>
        <div className="flex flex-col gap-1"><div className="type-callout whitespace-pre-wrap break-words">{p.message}</div><div className="type-footnote text-muted-foreground"><span className="font-mono">{p.hash.slice(0, 10)}</span> · {p.author}{p.at && ` · ${tm(p.at)}`} · {statLast(p.stat)}</div></div>
        <Diff diff={p.diff} truncated={p.truncated} /></div>
    : p.type === "branch" ? <div className="flex flex-col gap-3" data-out-branch>
        <div className="flex items-center gap-1.5 type-footnote text-muted-foreground"><GitBranchIcon className="size-3.5" /><span className="font-mono">{p.branch}</span>
          {p.base ? <span>{t("pages:outputs.branchAhead", { base: p.base, count: p.commits.length })}{p.stat && ` · ${statLast(p.stat)}`}</span> : <span>{t("pages:outputs.branchRecent")}</span>}</div>
        {p.base && !p.commits.length && <Note>{t("pages:outputs.merged", { base: p.base })}</Note>}
        {p.commits.length > 0 && <Commits cs={p.commits} />}
        <Diff diff={p.diff} truncated={p.truncated} /></div>
    : p.type === "image" ? (p.large ? <Missing reason={t("pages:outputs.tooLarge", { size: size(p.size ?? 0) })} />
        : <Gallery pics={pics} at={cur} go={goPic} notesOf={notesOf} drawing={drawing} focus={focus} mode={mode} setMode={setMode}
            onDraw={(x, b) => setPending({ art: x.art, path: x.path, anchor: { type: "region", ...b } })} />)
    : <Media p={p} url={raw()} page={page} />
  return (
    <div className="flex min-h-0 min-w-0 flex-col gap-3 overflow-y-auto" data-out-preview={p?.type ?? "loading"} data-kind={k}>
      {path.length > 0 && <nav className="flex flex-wrap items-center gap-1 type-footnote text-muted-foreground" data-out-crumbs>
        <button type="button" className="hover:text-foreground" onClick={() => setPath([])}>{(loc ?? "").replace(/\/$/, "").split("/").pop() || t("pages:outputs.output")}</button>
        {path.map((s, n) => <span key={n} className="flex items-center gap-1"><ChevronRightIcon className="size-3" /><button type="button" className={cn(n === path.length - 1 ? "text-foreground" : "hover:text-foreground")} onClick={() => setPath(path.slice(0, n + 1))}>{s}</button></span>)}
      </nav>}
      <div ref={box} className="min-w-0 shrink-0" data-out-body>{body}</div>
      {p && !err && p.type !== "missing" && <AnnBar item={item} art={art} path={at} notes={notes} allDrafts={drafts} pending={pending} setPending={setPending} sel={sel} clearSel={() => setSel(null)}
        canDraw={p.type === "image" && !p.large} drawing={drawing} setDrawing={setDrawing} pdf={p.type === "pdf"} onLocate={locate} />}
      {(where || file || ref) && <div className="flex flex-wrap items-center gap-2 border-t pt-3">
        {where && <span className="mr-auto min-w-0 truncate font-mono text-[12px] text-muted-foreground" title={where}>{where}{p && "size" in p && p.size != null ? ` · ${size(p.size)}` : ""}</span>}
        {file && <Button variant="ghost" size="sm" onClick={() => { location.href = raw(true) }}><DownloadIcon />{t("pages:outputs.download")}</Button>}
        {media && <Button variant="ghost" size="sm" onClick={() => window.open(raw(), "_blank", "noopener")}><ExternalLinkIcon />{t("pages:outputs.newTab")}</Button>}
        {ref && <Button variant="ghost" size="sm" className={cn(!where && "ml-auto")} onClick={() => { navigator.clipboard?.writeText(ref).catch(() => {}); toast(p?.type === "commit" ? t("pages:outputs.copiedCommit") : t("pages:outputs.copiedBranch")) }}><CopyIcon />{p?.type === "commit" ? t("pages:outputs.copyCommit") : t("pages:outputs.copyBranch")}</Button>}
        {where && <Button variant="ghost" size="sm" onClick={() => { navigator.clipboard?.writeText(where).catch(() => {}); toast(t("pages:outputs.copiedLocation")) }}><CopyIcon />{t("pages:outputs.copyLocation")}</Button>}
      </div>}
    </div>
  )
}
