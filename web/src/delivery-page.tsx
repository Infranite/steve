import { useEffect, useState } from "react"
import { CopyIcon, DownloadIcon, ExternalLinkIcon, FileTextIcon, RefreshCwIcon } from "lucide-react"
import { api, Unauthenticated, withWs } from "@/api"
import { size } from "@/attach"
import { Card, CardContent, CardHeader } from "@/components/affect/card-clean"
import { Button } from "@/components/ui/button"
import { Empty } from "@/components/ui/empty"
import { Spinner } from "@/components/ui/spinner"
import { currentHub, hubUrl } from "@/hub"
import { useT } from "@/i18n"
import { date } from "@/i18n/format"
import { OfficeView } from "@/office"
import { TextView } from "@/outputs"
import { deliveryContent, deliveryForm, deliveryTextLang, loopbackUrl, webUrl, type Delivery } from "@/delivery"
import { DeliveryHttpError, previewText, sharedDelivery } from "@/delivery-data"
import { DeliveryShares } from "@/delivery-shares"

// API URLs may already bind a workspace. Preserve that scope instead of adding a second, conflicting ws value.
const scoped = (url: string) => new URL(url, currentHub()).searchParams.has("ws") ? url : withWs(url)

function CannotPreview({ reason }: { reason?: string }) {
  const t = useT()
  return <Empty title={t("pages:delivery.noPreview")} text={reason || t("pages:delivery.downloadHint")} data-delivery-no-preview />
}

function Content({ d, shared }: { d: Delivery; shared: boolean }) {
  const t = useT(), base = shared ? location.origin : currentHub()
  const content = deliveryContent(d, base), url = content && (shared ? content : scoped(content))
  const form = d.preview?.type === "text" && deliveryForm(d) !== "html" ? "text" : deliveryForm(d)
  const [text, setText] = useState<Delivery["preview"]>(null), [error, setError] = useState(""), [reset, setReset] = useState(0)
  useEffect(() => {
    const controller = new AbortController()
    setError(""); setText(null)
    if (form !== "text" || !url) return () => controller.abort()
    if (d.preview?.type === "text") { setText(d.preview); return () => controller.abort() }
    fetch(url, { credentials: shared ? "omit" : "include", signal: AbortSignal.any([controller.signal, AbortSignal.timeout(30_000)]), cache: "no-store" })
      .then((r) => previewText(r)).then((p) => { if (!controller.signal.aborted) setText({ type: "text", lang: deliveryTextLang(d), ...p }) }, () => {
        if (!controller.signal.aborted) setError(t("pages:delivery.contentError"))
      })
    return () => controller.abort()
  }, [d, url, form, shared, t])
  if (!url) return <CannotPreview reason={t("pages:delivery.invalidUrl")} />
  if (error) return <div role="alert"><CannotPreview reason={error} /></div>
  if (form === "html") return <div className="flex min-w-0 flex-col gap-2">
    <div className="flex flex-wrap items-center justify-between gap-2">
      <p className="type-footnote text-muted-foreground">{t("pages:delivery.staticHtml")}</p>
      <Button size="sm" variant="ghost" onClick={() => setReset((x) => x + 1)} data-html-reset><RefreshCwIcon />{t("pages:delivery.resetPreview")}</Button>
    </div>
    {/* Both signed-in and shared HTML are static. Same-origin preserves cookies on signed-in relative requests;
        scripts stay disabled by this iframe AND the Hub's response CSP (sandbox allow-same-origin; script-src 'none').
        Keep the real URL as the base for bundled assets and page jumps. No scripts, top navigation, forms or popups. */}
    <iframe key={reset} title={d.title || d.name} src={url} sandbox="allow-same-origin" referrerPolicy="no-referrer" className="block h-[70dvh] min-h-72 w-full rounded-xl bg-white ring-1 ring-border" data-delivery-html />
  </div>
  if (form === "image") return <img src={url} alt={d.title || d.name} onError={() => setError(t("pages:delivery.contentError"))} className="mx-auto block max-h-[75dvh] max-w-full rounded-lg object-contain" referrerPolicy="no-referrer" data-delivery-image />
  if (form === "pdf") return <iframe title={d.name} src={url} referrerPolicy="no-referrer" className="block h-[75dvh] min-h-72 w-full rounded-xl ring-1 ring-border" data-delivery-pdf />
  if (form === "office") return <OfficeView url={url} name={d.name} credentials={shared ? "omit" : "include"} fallback={(reason) => <CannotPreview reason={reason} />} />
  if (form === "text") return text ? <TextView p={{ ...text, type: "text", name: d.name, path: "", size: d.size }} /> : <div role="status" className="py-8"><Spinner className="mx-auto size-5" /></div>
  return <CannotPreview />
}

export function DeliveryView({ id, shared = false, embedded = false }: { id: string; shared?: boolean; embedded?: boolean }) {
  const t = useT(), [d, setDelivery] = useState<Delivery | null>(null), [error, setError] = useState<unknown>(null)
  const [revision, setRevision] = useState(0), [copied, setCopied] = useState(false), [copyError, setCopyError] = useState(false)
  const [now, setNow] = useState(Date.now)
  useEffect(() => {
    const controller = new AbortController()
    let request = 0
    setDelivery(null); setError(null); setCopied(false); setCopyError(false)
    const load = () => shared ? sharedDelivery(id, AbortSignal.any([controller.signal, AbortSignal.timeout(30_000)])) : api.delivery(id)
    const fetchDelivery = () => {
      const current = ++request
      return load().then((value) => { if (!controller.signal.aborted && current === request) { setDelivery(value); setError(null); setNow(Date.now()) } }, (e) => { if (!controller.signal.aborted && current === request) { setDelivery(null); setError(e) } })
    }
    void fetchDelivery()
    // Recheck an open share after revocation; the server enforces access on every raw/content request immediately.
    const tick = shared ? window.setInterval(() => { setNow(Date.now()); void fetchDelivery() }, 30_000) : undefined
    const focus = () => { if (shared) void fetchDelivery() }
    window.addEventListener("focus", focus)
    return () => { controller.abort(); clearInterval(tick); window.removeEventListener("focus", focus) }
  }, [id, shared, revision])
  useEffect(() => {
    if (!shared || !d?.expires_at || d.expires_at <= now) return
    const timer = window.setTimeout(() => setNow(Date.now()), Math.max(0, Math.min(2_147_483_647, d.expires_at - Date.now() + 1)))
    return () => clearTimeout(timer)
  }, [shared, d?.expires_at, now])
  const denied = shared && error instanceof DeliveryHttpError && [401, 403, 404, 410].includes(error.status)
  const expired = shared && d?.expires_at != null && d.expires_at <= now
  if (error || expired) return <div className="mx-auto flex max-w-xl flex-col gap-3 px-4 py-12" role="alert" data-delivery-error>
    <h1 className="type-title">{t(denied || expired ? "pages:delivery.shareUnavailable" : "pages:delivery.loadError")}</h1>
    <p className="wrap-anywhere type-callout text-muted-foreground">{t(shared ? (denied || expired ? "pages:delivery.shareUnavailableHint" : "pages:delivery.unreachable") : "pages:delivery.loadErrorHint")}</p>
    {!shared && error instanceof Error && <p className="wrap-anywhere type-footnote text-muted-foreground">{error.message}</p>}
    {!shared && error instanceof Unauthenticated && <Button render={<a href={hubUrl(error.login ?? "/auth/login") + "?back=" + encodeURIComponent(location.pathname + location.search)} />} nativeButton={false}>{t("shell:app.signIn")}</Button>}
    <Button className="self-start" variant="outline" onClick={() => setRevision((n) => n + 1)} data-delivery-retry>{t("common:action.retry")}</Button>
  </div>
  if (!d) return <div className="flex justify-center p-12" role="status"><Spinner className="size-5" /></div>
  const base = shared ? location.origin : currentHub(), canonical = webUrl(d.url, base), download = webUrl(d.download_url, base)
  const url = canonical && (shared ? canonical : scoped(canonical))
  const copy = async () => {
    if (!url) return
    try { await navigator.clipboard.writeText(url); setCopied(true); setCopyError(false) } catch { setCopyError(true) }
  }
  return <article className="mx-auto flex w-full min-w-0 max-w-6xl flex-col gap-5 p-4 sm:p-6" data-delivery={d.id} data-delivery-shared={shared}>
    <header className="flex min-w-0 flex-col gap-2">
      <p className="type-label text-muted-foreground">{t(shared ? "pages:delivery.sharedTitle" : "pages:delivery.title")}</p>
      <h1 className="wrap-anywhere type-title">{d.title || d.name}</h1>
      <p className="wrap-anywhere type-callout text-muted-foreground" data-delivery-name>{d.name}</p>
      <p className="wrap-anywhere type-footnote text-muted-foreground">{d.mime} · {size(d.size)} · {date(d.created, { dateStyle: "medium", timeStyle: "short" })}</p>
      {!shared && <a href={withWs("/") + `#/items/${encodeURIComponent(d.item_id)}`} className="self-start type-callout text-blue underline underline-offset-2" data-delivery-item>{t("pages:delivery.fromItem", { item: d.item_id })}</a>}
      {shared && d.expires_at != null && <p className="type-footnote text-muted-foreground">{t("pages:delivery.expiresAt", { time: date(d.expires_at, { dateStyle: "medium", timeStyle: "short" }) })}</p>}
      <div className="flex flex-wrap gap-2 pt-1">
        {download && <Button render={<a href={shared ? download : scoped(download)} download={d.name} referrerPolicy="no-referrer" />} nativeButton={false} data-delivery-download><DownloadIcon />{t("pages:delivery.download")}</Button>}
        {embedded && url && <Button variant="outline" render={<a href={url} target="_blank" rel="noreferrer" />} nativeButton={false} data-delivery-open><ExternalLinkIcon />{t("pages:delivery.open")}</Button>}
        <Button variant="outline" onClick={() => void copy()} disabled={!url} data-delivery-copy><CopyIcon />{t(copied ? "pages:delivery.copied" : "pages:delivery.copyLink")}</Button>
      </div>
      {copyError && <p className="type-footnote text-destructive" role="alert">{t("pages:delivery.copyFailed")}</p>}
      {(!url || !download) && <p role="alert" className="type-callout text-attention">{t("pages:delivery.invalidUrl")}</p>}
      {url && loopbackUrl(url) && <p className="type-callout text-attention" data-loopback-warning>{t("pages:delivery.loopback")}</p>}
      {d.access_warning && <p className="wrap-anywhere type-callout text-attention" role="note" data-access-warning>{d.access_warning}</p>}
      {!shared && <p className="type-footnote text-muted-foreground">{t("pages:delivery.loginRequired")}</p>}
    </header>
    <Card as="section"><CardHeader title={t("pages:outputs.preview")} titleAs="h2" icon={<FileTextIcon />} /><CardContent className="min-w-0"><Content key={id} d={d} shared={shared} /></CardContent></Card>
    {!shared && !embedded && <DeliveryShares key={id} id={id} />}
  </article>
}

export function DeliveryPage({ id, shared = false }: { id: string; shared?: boolean }) {
  return <main className="h-[100dvh] overflow-y-auto bg-background text-foreground" data-delivery-page><DeliveryView id={id} shared={shared} /></main>
}
