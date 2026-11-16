/* Stable deliveries are Hub-owned files. An artifact id or a Node path is never a delivery id. */
export type Delivery = {
  id: string; item_id: string; title: string; name: string; mime: string; size: number; created: number
  entrypoint: string | null; url: string; download_url: string; content_url: string; access_warning: string | null
  /** Authenticated metadata supplies the verified owner workspace; anonymous shares omit it. */
  workspace_id?: string
  /** Optional existing preview contract, especially for server-converted documents. */
  preview?: { type: "text"; lang: string; text: string; truncated: boolean; from?: string } | null
  /** A share may additionally report its deadline, so an already-open page can retire its preview. */
  expires_at?: number
}
export type DeliveryShare = { token: string; url: string; expires_at: number; revoked_at?: number | null }
export type DeliveryRoute = { id: string; shared: boolean }

/** Bind a standalone page before App loads /api/state. Never substitute the browser's current workspace. */
export function deliveryWorkspace(d: Pick<Delivery, "workspace_id">, explicit: string | null): string | null {
  const ws = d.workspace_id
  return typeof ws === "string" && ws.length > 0 && (explicit === null || explicit === ws) ? ws : null
}

export function deliveryRoute(path: string): DeliveryRoute | null {
  const m = /^\/(deliveries|shares)\/([^/]+)\/?$/.exec(path)
  if (!m) return null
  try { return { id: decodeURIComponent(m[2]), shared: m[1] === "shares" } } catch { return null }
}

/** Only API URL fields belong here, never artifact locations or Node paths. Only http(s) links are allowed. */
export function webUrl(value: string | null | undefined, base: string): string | null {
  if (!value || (!/^https?:\/\//i.test(value) && !/^\/(?!\/)/.test(value))) return null
  try {
    const u = new URL(value, base)
    return /^https?:$/.test(u.protocol) && !u.username && !u.password ? u.href : null
  } catch { return null }
}

export function loopbackUrl(value: string): boolean {
  try {
    const h = new URL(value).hostname.toLowerCase().replace(/^\[|\]$/g, "").replace(/\.$/, "")
    return h === "localhost" || h.endsWith(".localhost") || h === "::1" || h === "0.0.0.0" || h === "::" || /^127\./.test(h) || /^::ffff:(127\.|7f[0-9a-f]{2}:)/.test(h)
  } catch { return false }
}

/** content_url is an inline resource URL, or the /content[/] root for an HTML bundle. Preserve its query. */
export function deliveryContent(d: Delivery, base: string): string | null {
  const value = webUrl(d.content_url, base)
  if (!value || !d.entrypoint) return value
  const parts = d.entrypoint.split("/")
  if (parts.some((p) => !p || p === "." || p === ".." || /[\\?#]/.test(p))) return null
  const u = new URL(value)
  if (/\/content\/?$/.test(u.pathname) || u.pathname.endsWith("/")) {
    u.pathname = u.pathname.replace(/\/?$/, "/") + parts.map(encodeURIComponent).join("/")
  }
  return u.href
}

export type DeliveryForm = "html" | "image" | "pdf" | "office" | "text" | "binary"
export function deliveryForm(d: Pick<Delivery, "name" | "mime" | "entrypoint">): DeliveryForm {
  const mime = d.mime.split(";")[0].toLowerCase()
  if (d.entrypoint || mime === "text/html" || /\.html?$/i.test(d.name)) return "html"
  if (mime.startsWith("image/")) return "image"
  if (mime === "application/pdf" || /\.pdf$/i.test(d.name)) return "pdf"
  if (/\.(docx|xlsx|pptx)$/i.test(d.name)) return "office"
  if (mime.startsWith("text/") || /\.(md|markdown|txt|log|json|csv|tsv|ya?ml|xml)$/i.test(d.name)) return "text"
  return "binary"
}

export function deliveryTextLang(d: Pick<Delivery, "name" | "mime">): string {
  if (/\.(md|markdown)$/i.test(d.name) || /markdown/i.test(d.mime)) return "markdown"
  return /\.(json|csv|tsv|xml)$/i.exec(d.name)?.[1].toLowerCase() ?? "text"
}

/** New artifact payloads add delivery_id; older local paths stay in the existing workspace preview. */
export function outputDeliveryId(o: readonly unknown[]): string | null {
  if (typeof o[6] === "string" && o[6]) return o[6]
  if (typeof o[2] !== "string") return null
  return deliveryRoute(o[2])?.shared === false ? deliveryRoute(o[2])!.id : null
}
