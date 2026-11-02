/* Attachments: upload state for a composer, and how attached files look wherever a message shows
 * (chat bubbles, item comments, session transcripts). Images preview and zoom; other files are a chip that downloads. */
import { useEffect, useRef, useState, useSyncExternalStore } from "react"
import { DownloadIcon, FileArchiveIcon, FileIcon, FileTextIcon, PaperclipIcon, XIcon } from "lucide-react"
import { cn } from "cn"
import { api, fileUrl, type RFile } from "@/api"
import { Attachment, AttachmentAction, AttachmentActions, AttachmentContent, AttachmentDescription, AttachmentMedia, AttachmentTitle } from "@/components/ui/attachment"
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog"
import { Spinner } from "@/components/ui/spinner"

export const MAX_FILE = 20 * 1024 * 1024, MAX_FILES = 10
export const ACCEPT = "image/png,image/jpeg,image/gif,image/webp,application/pdf,application/zip,.zip,.pdf,.txt,.log,.md,.csv,.json,.yaml,.yml,.xml,text/*"
const IMG = ["image/png", "image/jpeg", "image/gif", "image/webp"]
export const isImage = (mime: string) => IMG.includes(mime.split(";")[0])
export const size = (n: number) => (n >= 1024 * 1024 ? (n / 1024 / 1024).toFixed(1) + " MB" : Math.max(1, Math.round(n / 1024)) + " KB")

/* ── composer side ── */
export type Pending = { key: string; name: string; mime: string; size: number; preview?: string; file?: RFile; error?: string }
let seq = 0
type UploadState = { list: Pending[]; listeners: Set<() => void> }
const drafts = new Map<string, UploadState>()
const stamp = () => { const d = new Date(), p = (x: number) => String(x).padStart(2, "0"); return `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}` }
/** files picked, pasted or dropped: uploaded right away, sent by id with the message */
export function useUploads(initial: RFile[] = [], draftKey?: string, onReady?: (files: RFile[]) => void) {
  const [state] = useState<UploadState>(() => {
    const saved = draftKey ? drafts.get(draftKey) : undefined
    if (saved) return saved
    const created = { list: initial.map((f) => ({ key: "u" + ++seq, name: f.name, mime: f.mime, size: f.size, file: f })), listeners: new Set<() => void>() }
    if (draftKey) drafts.set(draftKey, created)
    return created
  })
  const list = useSyncExternalStore((f) => { state.listeners.add(f); return () => { state.listeners.delete(f) } }, () => state.list)
  const live = { get current() { return state.list } }
  const setList = (update: Pending[] | ((list: Pending[]) => Pending[])) => {
    state.list = typeof update === "function" ? update(state.list) : update
    onReady?.(state.list.flatMap((p) => p.file ? [p.file] : []))
    state.listeners.forEach((f) => f())
  }
  /* A keyed composer's upload can finish while its page is closed; preserve that upload and its preview. */
  useEffect(() => () => { if (!draftKey) for (const p of state.list) if (p.preview) URL.revokeObjectURL(p.preview) }, [])
  const patch = (key: string, p: Partial<Pending>) => setList((xs) => xs.map((x) => (x.key === key ? { ...x, ...p } : x)))
  const add = (fs: File[] | FileList) => {
    const room = MAX_FILES - live.current.length
    const picked = [...fs].slice(0, Math.max(0, room)).map((f) => {
      /* a pasted screenshot is always "image.png": give it a name that says when */
      const name = /^image\.(png|jpe?g|gif|webp)$/i.test(f.name) ? `截图-${stamp()}.${f.name.split(".").pop()}` : f.name || "file"
      const p: Pending = { key: "u" + ++seq, name, mime: f.type, size: f.size, preview: isImage(f.type) ? URL.createObjectURL(f) : undefined }
      if (f.size > MAX_FILE) p.error = `超过 ${MAX_FILE / 1024 / 1024} MB`
      return { p, f }
    })
    if (!picked.length) return
    setList((xs) => [...xs, ...picked.map((x) => x.p)])
    for (const { p, f } of picked) if (!p.error) api.upload(f, p.name).then((r) => patch(p.key, { file: r, name: r.name, mime: r.mime }), (e) => patch(p.key, { error: (e as Error).message }))
  }
  /** files already uploaded (a withdrawn queued message going back into the composer) */
  const put = (fs: RFile[]) => setList((xs) => [...xs, ...fs.filter((f) => !xs.some((x) => x.file?.id === f.id)).map((f) => ({ key: "u" + ++seq, name: f.name, mime: f.mime, size: f.size, file: f }))])
  const remove = (key: string) => setList((xs) => { const x = xs.find((y) => y.key === key); if (x?.preview) URL.revokeObjectURL(x.preview); return xs.filter((y) => y.key !== key) })
  const clear = () => { for (const p of live.current) if (p.preview) URL.revokeObjectURL(p.preview); setList([]) }
  const uploading = list.some((p) => !p.file && !p.error)
  const ready = list.filter((p) => p.file).map((p) => p.file!)
  return { list, add, put, remove, clear, uploading, ready }
}

/** thumbnails in the composer, each removable before sending */
export function PendingList({ list, onRemove }: { list: Pending[]; onRemove: (key: string) => void }) {
  return (
    <div className="flex flex-wrap gap-2" data-attachments>
      {list.map((p) => (
        <Attachment key={p.key} size="xs" state={p.error ? "error" : p.file ? "done" : "uploading"} className="max-w-[220px] pr-1" data-pending-file={p.file?.id ?? ""} data-state-label={p.error ? "error" : p.file ? "done" : "uploading"}>
          <AttachmentMedia variant={p.preview ? "image" : "icon"}>{p.preview ? <img src={p.preview} alt={p.name} /> : !p.file && !p.error ? <Spinner /> : <KindIcon mime={p.mime} />}</AttachmentMedia>
          <AttachmentContent><AttachmentTitle>{p.name}</AttachmentTitle><AttachmentDescription>{p.error ?? (p.file ? size(p.size) : "上传中…")}</AttachmentDescription></AttachmentContent>
          <AttachmentActions><AttachmentAction aria-label={"移除 " + p.name} onClick={() => onRemove(p.key)} data-remove-file><XIcon /></AttachmentAction></AttachmentActions>
        </Attachment>
      ))}
    </div>
  )
}

export function AttachButton({ onFiles, disabled }: { onFiles: (f: FileList) => void; disabled?: boolean }) {
  const ref = useRef<HTMLInputElement>(null)
  return (
    <>
      <button type="button" aria-label="添加附件" title="添加附件（也可以粘贴截图或拖进来）" disabled={disabled} onClick={() => ref.current?.click()} data-attach
        className="flex size-8 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-40"><PaperclipIcon className="size-4" strokeWidth={1.75} /></button>
      <input ref={ref} type="file" multiple accept={ACCEPT} className="hidden" data-file-input onChange={(e) => { if (e.target.files?.length) onFiles(e.target.files); e.target.value = "" }} />
    </>
  )
}

/* ── message side ── */
function KindIcon({ mime, className }: { mime: string; className?: string }) {
  const I = mime.startsWith("text/") ? FileTextIcon : mime === "application/zip" ? FileArchiveIcon : FileIcon
  return <I className={className} strokeWidth={1.5} />
}
/** attached files of one message: image previews (click to zoom) and download chips */
export function Files({ files, align = "start", className }: { files?: RFile[] | null; align?: "start" | "end"; className?: string }) {
  const [zoom, setZoom] = useState<RFile | null>(null)
  const xs = (files ?? []).filter((f) => f && typeof f === "object" && f.id)
  if (!xs.length) return null
  const imgs = xs.filter((f) => isImage(f.mime)), rest = xs.filter((f) => !isImage(f.mime))
  return (
    <div className={cn("flex flex-col gap-1.5", align === "end" ? "items-end" : "items-start", className)} data-files>
      {imgs.length > 0 && <div className={cn("flex flex-wrap gap-1.5", align === "end" && "justify-end")}>
        {imgs.map((f) => (
          <button key={f.id} type="button" onClick={() => setZoom(f)} aria-label={"放大查看 " + f.name} data-file-image={f.id}
            className="overflow-hidden rounded-xl border bg-muted outline-none focus-visible:ring-2 focus-visible:ring-ring/40">
            <img src={fileUrl(f.id)} alt={f.name} loading="lazy" className="block max-h-48 max-w-[min(320px,70vw)] object-contain" />
          </button>
        ))}
      </div>}
      {rest.length > 0 && <div className={cn("flex flex-wrap gap-1.5", align === "end" && "justify-end")}>
        {rest.map((f) => (
          <a key={f.id} href={fileUrl(f.id, true)} download={f.name} data-file-chip={f.id} className="inline-flex h-8 max-w-[260px] items-center gap-1.5 rounded-full border bg-card px-2.5 type-footnote hover:bg-muted">
            <KindIcon mime={f.mime} className="size-3.5 shrink-0 text-muted-foreground" /><span className="min-w-0 truncate">{f.name}</span><span className="shrink-0 text-muted-foreground">{size(f.size)}</span><DownloadIcon className="size-3.5 shrink-0 text-muted-foreground" />
          </a>
        ))}
      </div>}
      <Dialog open={!!zoom} onOpenChange={(o) => { if (!o) setZoom(null) }}>
        <DialogContent className="max-h-[92vh] w-auto max-w-[92vw] gap-2 p-2 sm:max-w-[92vw]" data-file-zoom>
          <DialogTitle className="sr-only">{zoom?.name}</DialogTitle>
          {zoom && <img src={fileUrl(zoom.id)} alt={zoom.name} className="max-h-[80vh] max-w-[88vw] rounded-lg object-contain" />}
          {zoom && <div className="flex items-center justify-between gap-3 px-1 type-footnote text-muted-foreground"><span className="min-w-0 truncate">{zoom.name} · {size(zoom.size)}</span>
            <a href={fileUrl(zoom.id, true)} download={zoom.name} className="inline-flex items-center gap-1 text-blue hover:underline"><DownloadIcon className="size-3.5" />下载</a></div>}
        </DialogContent>
      </Dialog>
    </div>
  )
}
