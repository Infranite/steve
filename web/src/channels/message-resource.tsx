import { useEffect, useRef, useState } from "react"
import { DownloadIcon, FileIcon, ImageIcon, RefreshCwIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog"
import { Spinner } from "@/components/ui/spinner"
import { wsHeaders } from "@/api"

const IMAGE_TYPES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"])
const MAX_BYTES = 25 * 1024 * 1024

class ResourceFailure extends Error {
  retry = false
  constructor(message: string, retry = false) { super(message); this.retry = retry }
}
const REASONS: Record<string, [string, boolean]> = {
  too_large: ["资源超过 25 MB，暂不支持在此打开", false], timeout: ["资源读取超时，请重试", true],
  busy: ["正在读取的资源较多，请稍后重试", true], rate_limited: ["飞书读取频率受限，请稍后重试", true],
  no_permission: ["接入账号缺少读取权限，请检查飞书授权", false], not_in_chat: ["接入账号不在这个会话中", false],
  restricted: ["飞书限制了此资源的访问，请在飞书中查看", false], invisible: ["接入账号无法读取这条历史消息", false],
  gone: ["资源已删除或不属于这条消息", false], disconnected: ["接入账号已断开", false],
  folder: ["文件夹请在飞书中打开", false], sticker: ["表情包请在飞书中查看", false],
  no_key: ["消息未提供可读取的资源", false], unsupported: ["此接入暂不支持读取资源", false],
}

async function readResource(url: string, signal: AbortSignal, image: boolean): Promise<Blob> {
  const r = await fetch(url, { headers: wsHeaders(), credentials: "same-origin", cache: "no-store", signal })
  if (!r.ok) {
    const message = r.status === 401 ? "登录已失效，请重新登录" : r.status === 403 ? "没有读取此资源的权限" : r.status === 404 || r.status === 410 ? "资源已不可用或已过保留期限" : "暂时无法读取资源，请重试"
    const body = await r.json().catch(() => ({}))
    const known = typeof body.reason === "string" ? REASONS[body.reason] : undefined
    throw new ResourceFailure(known?.[0] ?? message, known?.[1] ?? (r.status >= 500 || r.status === 429))
  }
  const mime = (r.headers.get("content-type") ?? "application/octet-stream").split(";")[0].trim().toLowerCase()
  if (image && !IMAGE_TYPES.has(mime)) throw new Error("这个资源不是可预览的图片")
  if (Number(r.headers.get("content-length")) > MAX_BYTES) throw new Error("资源超过 25 MB，暂不支持在此打开")
  const reader = r.body?.getReader()
  if (!reader) throw new Error("资源响应不完整，请重试")
  const parts: Uint8Array<ArrayBuffer>[] = []; let total = 0
  try {
    while (true) {
      const { value, done } = await reader.read()
      if (done) break
      total += value.byteLength
      if (total > MAX_BYTES) throw new Error("资源超过 25 MB，暂不支持在此打开")
      parts.push(new Uint8Array(value))
    }
  } finally { await reader.cancel().catch(() => {}) }
  if (!total) throw new Error("资源内容为空")
  return new Blob(parts, { type: image ? mime : "application/octet-stream" })
}

export function MessageResource({ url, name, image = false, unavailable }: { url?: string; name: string; image?: boolean; unavailable?: string }) {
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
    }).catch(e => { if (!controller.signal.aborted) { setError(e.name === "TimeoutError" ? "图片读取超时，请重试" : e instanceof TypeError ? "网络连接失败，请重试" : e.message || "图片读取失败"); setRetryable(e instanceof ResourceFailure ? e.retry : e.name === "TimeoutError" || e instanceof TypeError) } })
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
      a.href = href; a.download = name.replace(/[\\/\u0000-\u001f\u007f]/g, "_") || "附件"; a.click()
      setTimeout(() => URL.revokeObjectURL(href), 1000)
    } catch (e) { if (!controller.signal.aborted) setError(e instanceof TypeError ? "网络连接失败，请重试" : e instanceof Error ? e.message : "下载失败，请重试") }
    finally { if (!controller.signal.aborted) setBusy(false) }
  }
  return <div ref={root} className="min-w-0 max-w-full rounded-lg border bg-background p-2" data-message-resource={image ? "image" : "file"}>
    {image && preview && !error && <button type="button" onClick={() => setOpen(true)} aria-label={`查看图片：${name}`} className="block max-w-full rounded focus-visible:ring-2 focus-visible:ring-ring">
      <img src={preview} alt={name} className="max-h-64 max-w-full rounded object-contain" onError={() => setError("图片无法解码，请下载后查看")} />
    </button>}
    <div className="flex min-w-0 items-center gap-2 type-footnote">
      {image ? <ImageIcon className="size-4 shrink-0" /> : <FileIcon className="size-4 shrink-0" />}
      <span className="min-w-0 flex-1 break-all">{name}</span>
      {url && <Button size="xs" variant="ghost" disabled={busy} onClick={() => void save()} aria-label={`下载：${name}`}>
        {busy ? <Spinner /> : <DownloadIcon />}下载
      </Button>}
    </div>
    {!url && <p className="mt-1 type-footnote text-muted-foreground">{unavailable || "此资源暂无法读取，请在飞书中查看"}</p>}
    {error && <div role="status" className="mt-1 flex flex-wrap items-center gap-1 type-footnote text-muted-foreground">
      <span>{error}</span>{image && retryable && <Button size="xs" variant="ghost" onClick={() => setAttempt(x => x + 1)}><RefreshCwIcon />重试</Button>}
    </div>}
    {image && <Dialog open={open} onOpenChange={setOpen}><DialogContent className="max-w-[min(92vw,1000px)]">
      <DialogTitle className="break-all">{name}</DialogTitle>
      {preview && <img src={preview} alt={name} className="max-h-[75vh] max-w-full object-contain" />}
    </DialogContent></Dialog>}
  </div>
}
