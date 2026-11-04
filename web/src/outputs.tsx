import { api, outRawUrl, type ROutPreview } from "@/api"
import { size } from "@/attach"
import { Markdown } from "@/components/markdown"
import { Button } from "@/components/ui/button"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { Spinner } from "@/components/ui/spinner"
import { tm, toast, type Out } from "@/store"
import { Segmented } from "@/ui"
import { cn } from "cn"
import { ChevronRightIcon, CopyIcon, DownloadIcon, ExternalLinkIcon, FileIcon, FileTextIcon, FolderIcon, GitBranchIcon } from "lucide-react"
import { useEffect, useState, type ReactNode } from "react"

/* what an item's output holds, shown as itself: documents rendered, code highlighted, tables as tables, images,
 * PDFs and media in place, folders browsable, branches and commits as their commits and diff */

/** above this, code is shown plain: highlighting half a megabyte freezes the page */
const HIGHLIGHT_MAX = 120_000
const fence = (lang: string, t: string) => { const n = Math.max(3, ...[...t.matchAll(/`{3,}/g)].map((m) => m[0].length + 1)); const f = "`".repeat(n); return `${f}${lang}\n${t}\n${f}` }

function Note({ children }: { children: ReactNode }) {
  return <div className="rounded-lg bg-muted/60 px-3 py-2 type-footnote text-muted-foreground">{children}</div>
}
function Missing({ reason }: { reason: string }) {
  return <div className="flex flex-col items-center gap-1 py-12 text-center" data-out-missing><FileIcon className="size-6 text-tertiary" /><div className="type-callout">看不了内容</div><div className="max-w-md type-footnote text-muted-foreground">{reason}</div></div>
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
  return <iframe title="HTML 预览" sandbox="allow-scripts" referrerPolicy="no-referrer" srcDoc={text} className="block h-[60vh] w-full rounded-xl border bg-white" data-out-html />
}

/** a text file by what it is: a document, a page, a table, a diagram, JSON or code; rendered ones can show their source */
function TextView({ p }: { p: Extract<ROutPreview, { type: "text" }> }) {
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
      {rendered && <Segmented size="sm" value={tab} onChange={setTab} options={["预览", "源码"] as const} />}
      {p.from && <Note>工作区已收起，内容取自 {p.from}</Note>}
      {body}
      {p.truncated && <Note>文件较大，只显示了前 512 KB</Note>}
    </div>
  )
}

/** a unified diff, one collapsible block per file */
function Diff({ diff, truncated }: { diff: string; truncated: boolean }) {
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
              )) : <div className="px-3 text-muted-foreground">二进制文件或只改了权限</div>}</pre>
            </CollapsibleContent>
          </Collapsible>
        )
      })}
      {truncated && <Note>改动较多，只显示了前 400 KB</Note>}
    </div>
  )
}
const statLast = (stat: string) => stat.trim().split("\n").pop() ?? ""

/** `base`: the path walked below the output so far; an entry opens as `base/name` */
function Folder({ p, item, art, base, open }: { p: Extract<ROutPreview, { type: "dir" }>; item: string; art: string; base: string; open: (sub: string) => void }) {
  const at = (n: string) => (base ? base + "/" : "") + n
  const pics = p.entries.filter((e) => e.form === "image"), rest = p.entries.filter((e) => e.form !== "image")
  if (!p.entries.length) return <Missing reason="文件夹是空的" />
  return (
    <div className="flex flex-col gap-3" data-out-dir>
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
      {p.truncated && <Note>只列出了前 500 项</Note>}
    </div>
  )
}
function Media({ p, url }: { p: Extract<ROutPreview, { type: "image" | "pdf" | "video" | "audio" }>; url: string }) {
  if (p.large) return <Missing reason={`文件太大（${size(p.size ?? 0)}），请下载后查看`} />
  return p.type === "image" ? <a href={url} target="_blank" rel="noopener" className="block"><img src={url} alt={p.name} className="mx-auto max-h-[65vh] max-w-full rounded-lg border object-contain" data-out-image /></a>
    : p.type === "pdf" ? <iframe title={p.name} src={url} className="block h-[65vh] w-full rounded-xl border" data-out-pdf />
    : p.type === "video" ? <video src={url} controls className="max-h-[65vh] w-full rounded-xl bg-black" data-out-video />
    : <audio src={url} controls className="w-full" data-out-audio />
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

/** the body of the output dialog; `loc` and the kind come from the row, the rest from the Hub */
export function OutPreview({ o, item }: { o: Out; item: string }) {
  const [k, , loc, art] = o
  const [path, setPath] = useState<string[]>([])
  const [p, setP] = useState<ROutPreview | null>(null), [err, setErr] = useState("")
  const at = path.join("/")
  useEffect(() => {
    let live = true; setP(null); setErr("")
    api.outPreview(item, art, at || undefined).then((v) => live && setP(v), (e: Error) => live && setErr(e.message))
    return () => { live = false }
  }, [item, art, at])
  const media = !!p && ["image", "pdf", "video", "audio"].includes(p.type)
  /* a text git still had (its worktree is gone) has no file to download */
  const file = media || p?.type === "binary" || (p?.type === "text" && !p.from)
  const raw = (dl = false) => outRawUrl(item, art, at || undefined, dl)
  const where = p && "path" in p ? p.path : loc
  /* what a branch or commit is known by, to paste into git */
  const ref = p?.type === "commit" ? p.hash : p?.type === "branch" ? p.branch : null
  const body = err ? <Missing reason={err} />
    : !p ? <div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div>
    : p.type === "missing" ? <Missing reason={p.reason} />
    : p.type === "link" ? <div className="flex flex-col items-start gap-3 py-2"><span className="break-all font-mono text-[12.5px]">{p.url}</span><Button size="sm" onClick={() => window.open(p.url, "_blank", "noopener")}><ExternalLinkIcon />打开链接</Button></div>
    : p.type === "text" ? <TextView key={at} p={p} />
    : p.type === "binary" ? <Missing reason={`${p.name} 不是文本、图片或媒体文件（${size(p.size ?? 0)}），可以下载后用本地应用打开`} />
    : p.type === "dir" ? <Folder p={p} item={item} art={art} base={at} open={(s) => setPath(s.split("/"))} />
    : p.type === "commit" ? <div className="flex flex-col gap-3" data-out-commit>
        <div className="flex flex-col gap-1"><div className="type-callout whitespace-pre-wrap break-words">{p.message}</div><div className="type-footnote text-muted-foreground"><span className="font-mono">{p.hash.slice(0, 10)}</span> · {p.author}{p.at && ` · ${tm(p.at)}`} · {statLast(p.stat)}</div></div>
        <Diff diff={p.diff} truncated={p.truncated} /></div>
    : p.type === "branch" ? <div className="flex flex-col gap-3" data-out-branch>
        <div className="flex items-center gap-1.5 type-footnote text-muted-foreground"><GitBranchIcon className="size-3.5" /><span className="font-mono">{p.branch}</span>
          {p.base ? <span>· 比 {p.base} 多 {p.commits.length} 个提交{p.stat && ` · ${statLast(p.stat)}`}</span> : <span>· 最近的提交</span>}</div>
        {p.base && !p.commits.length && <Note>这个分支的改动已经都在 {p.base} 上了</Note>}
        {p.commits.length > 0 && <Commits cs={p.commits} />}
        <Diff diff={p.diff} truncated={p.truncated} /></div>
    : <Media p={p} url={raw()} />
  return (
    <div className="flex min-h-0 min-w-0 flex-col gap-3" data-out-preview={p?.type ?? "loading"} data-kind={k}>
      {path.length > 0 && <nav className="flex flex-wrap items-center gap-1 type-footnote text-muted-foreground" data-out-crumbs>
        <button type="button" className="hover:text-foreground" onClick={() => setPath([])}>{(loc ?? "").replace(/\/$/, "").split("/").pop() || "产出"}</button>
        {path.map((s, n) => <span key={n} className="flex items-center gap-1"><ChevronRightIcon className="size-3" /><button type="button" className={cn(n === path.length - 1 ? "text-foreground" : "hover:text-foreground")} onClick={() => setPath(path.slice(0, n + 1))}>{s}</button></span>)}
      </nav>}
      <div className="min-h-0 min-w-0 overflow-auto">{body}</div>
      {(where || file || ref) && <div className="flex flex-wrap items-center gap-2 border-t pt-3">
        {where && <span className="mr-auto min-w-0 truncate font-mono text-[12px] text-muted-foreground" title={where}>{where}{p && "size" in p && p.size != null ? ` · ${size(p.size)}` : ""}</span>}
        {file && <Button variant="ghost" size="sm" onClick={() => { location.href = raw(true) }}><DownloadIcon />下载</Button>}
        {media && <Button variant="ghost" size="sm" onClick={() => window.open(raw(), "_blank", "noopener")}><ExternalLinkIcon />新标签打开</Button>}
        {ref && <Button variant="ghost" size="sm" className={cn(!where && "ml-auto")} onClick={() => { navigator.clipboard?.writeText(ref).catch(() => {}); toast(p?.type === "commit" ? "已复制提交号" : "已复制分支名") }}><CopyIcon />{p?.type === "commit" ? "复制提交号" : "复制分支名"}</Button>}
        {where && <Button variant="ghost" size="sm" onClick={() => { navigator.clipboard?.writeText(where).catch(() => {}); toast("已复制位置") }}><CopyIcon />复制位置</Button>}
      </div>}
    </div>
  )
}
