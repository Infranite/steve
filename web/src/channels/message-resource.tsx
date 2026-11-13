import { hubUrl } from "@/hub"
import { useEffect, useRef, useState } from "react"
import { DownloadIcon, FileIcon, ImageIcon, RefreshCwIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog"
import { Spinner } from "@/components/ui/spinner"
import { wsHeaders } from "@/api"
import { t, useT } from "@/i18n"

const IMAGE_TYPES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"])
const MAX_BYTES = 25 * 1024 * 1024

class ResourceFailure extends Error {
  retry = false
  constructor(message: string, retry = false) { super(message); this.retry = retry }
}
/* the Hub's reason codes → [message key, worth retrying]; the message is resolved when the error is raised */
const REASONS = {
  too_large: ["channels:resource.tooLarge", false], timeout: ["channels:resource.timeout", true],
  busy: ["channels:resource.busy", true], rate_limited: ["channels:resource.rateLimited", true],
  no_permission: ["channels:resource.noPermission", false], not_in_chat: ["channels:resource.notInChat", false],
  restricted: ["channels:resource.restricted", false], invisible: ["channels:resource.invisible", false],
  gone: ["channels:resource.gone", false], disconnected: ["channels:resource.disconnected", false],
  folder: ["channels:resource.folder", false], sticker: ["channels:resource.sticker", false],
  no_key: ["channels:resource.noKey", false], unsupported: ["channels:resource.unsupported", false],
} as const

async function readResource(url: string, signal: AbortSignal, image: boolean): Promise<Blob> {
  const r = await fetch(hubUrl(url), { headers: wsHeaders(), credentials: "include", cache: "no-store", signal })
  if (!r.ok) {
    const message = t(r.status === 401 ? "channels:resource.signedOut" : r.status === 403 ? "channels:resource.forbidden" : r.status === 404 || r.status === 410 ? "channels:resource.notFound" : "channels:resource.failed")
    const body = await r.json().catch(() => ({}))
    const known = typeof body.reason === "string" && Object.hasOwn(REASONS, body.reason) ? REASONS[body.reason as keyof typeof REASONS] : undefined
    throw new ResourceFailure(known ? t(known[0]) : message, known?.[1] ?? (r.status >= 500 || r.status === 429))
  }
  const mime = (r.headers.get("content-type") ?? "application/octet-stream").split(";")[0].trim().toLowerCase()
  if (image && !IMAGE_TYPES.has(mime)) throw new Error(t("channels:resource.notImage"))
  if (Number(r.headers.get("content-length")) > MAX_BYTES) throw new Error(t("channels:resource.tooLarge"))
  const reader = r.body?.getReader()
  if (!reader) throw new Error(t("channels:resource.incomplete"))
  const parts: Uint8Array<ArrayBuffer>[] = []; let total = 0
  try {
    while (true) {
      const { value, done } = await reader.read()
      if (done) break
      total += value.byteLength
      if (total > MAX_BYTES) throw new Error(t("channels:resource.tooLarge"))
      parts.push(new Uint8Array(value))
    }
  } finally { await reader.cancel().catch(() => {}) }
  if (!total) throw new Error(t("channels:resource.empty"))
  return new Blob(parts, { type: image ? mime : "application/octet-stream" })
}

export function MessageResource({ url, name, image = false, unavailable }: { url?: string; name: string; image?: boolean; unavailable?: string }) {
  const tr = useT()
  const [preview, setPreview] = useState<string>(), [error, setError] = useState(""), [busy, setBusy] = useState(false)
  const [retryable, setRetryable] = useState(false)
  const [attempt, setAttempt] = useState(0), [open, setOpen] = useState(false), [visible, setVisible] = useState(false)
  const root = useRef<HTMLDivElement>(null), download = useRef<AbortController | null>(null)
  useEffect(() => {
    if (!image || !root.current) return
    const observer = new IntersectionObserver(([entry]) => { if (entry.isIntersecting) { setVisible(true); observer.disconnect() } }, { rootMargin: "200px" })
    observer.observe(root.current)
    return () => observer.disconnect()
  }, [image])
  useEffect(() => {
    setPreview(undefined); setError(""); setOpen(false); setRetryable(false)
    if (!url || !image || !visible) return
    const controller = new AbortController(); let objectUrl: string | undefined
    setBusy(true)
    readResource(url, AbortSignal.any([controller.signal, AbortSignal.timeout(30_000)]), true).then(blob => {
      if (controller.signal.aborted) return
      objectUrl = URL.createObjectURL(blob); setPreview(objectUrl)
    }).catch(e => { if (!controller.signal.aborted) { setError(e.name === "TimeoutError" ? t("channels:resource.imageTimeout") : e instanceof TypeError ? t("channels:resource.network") : e.message || t("channels:resource.imageFailed")); setRetryable(e instanceof ResourceFailure ? e.retry : e.name === "TimeoutError" || e instanceof TypeError) } })
      .finally(() => { if (!controller.signal.aborted) setBusy(false) })
    return () => { controller.abort(); if (objectUrl) URL.revokeObjectURL(objectUrl) }
  }, [url, image, visible, attempt])
  useEffect(() => () => { download.current?.abort() }, [url])
  const save = async () => {
    if (!url || busy) return
    const controller = new AbortController(); download.current = controller
    setBusy(true); setError("")
    try {
      const blob = await readResource(url, AbortSignal.any([controller.signal, AbortSignal.timeout(30_000)]), false)
      if (controller.signal.aborted) return
      const href = URL.createObjectURL(blob), a = document.createElement("a")
      a.href = href; a.download = name.replace(/[\\/\u0000-\u001f\u007f]/g, "_") || t("channels:resource.file"); a.click()
      setTimeout(() => URL.revokeObjectURL(href), 1000)
    } catch (e) { if (!controller.signal.aborted) setError(e instanceof TypeError ? t("channels:resource.network") : e instanceof Error ? e.message : t("channels:resource.downloadFailed")) }
    finally { if (!controller.signal.aborted) setBusy(false) }
  }
  return <div ref={root} className="min-w-0 max-w-full rounded-xl bg-muted/60 p-2.5" data-message-resource={image ? "image" : "file"}>
    {image && preview && !error && <button type="button" onClick={() => setOpen(true)} aria-label={tr("channels:resource.view", { name })} className="mb-2 block max-w-full rounded-lg focus-visible:ring-2 focus-visible:ring-ring">
      <img src={preview} alt={name} className="max-h-64 max-w-full rounded-lg object-contain" onError={() => setError(t("channels:resource.undecodable"))} />
    </button>}
    <div className="flex min-w-0 items-center gap-2 type-footnote">
      {image ? <ImageIcon className="size-4 shrink-0" /> : <FileIcon className="size-4 shrink-0" />}
      <span className="min-w-0 flex-1 break-all">{name}</span>
      {url && <Button size="xs" variant="ghost" disabled={busy} onClick={() => void save()} aria-label={tr("channels:resource.downloadAria", { name })}>
        {busy ? <Spinner /> : <DownloadIcon />}{tr("channels:resource.download")}
      </Button>}
    </div>
    {!url && <p className="mt-1 type-footnote text-muted-foreground">{unavailable || tr("channels:resource.unreadable")}</p>}
    {error && <div role="status" className="mt-1 flex flex-wrap items-center gap-1 type-footnote text-muted-foreground">
      <span>{error}</span>{image && retryable && <Button size="xs" variant="ghost" onClick={() => setAttempt(x => x + 1)}><RefreshCwIcon />{tr("common:action.retry")}</Button>}
    </div>}
    {image && <Dialog open={open} onOpenChange={setOpen}><DialogContent className="max-w-[min(92vw,1000px)]">
      <DialogTitle className="break-all">{name}</DialogTitle>
      {preview && <img src={preview} alt={name} className="max-h-[75vh] max-w-full object-contain" />}
    </DialogContent></Dialog>}
  </div>
}
