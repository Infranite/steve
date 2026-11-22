/* Stable deliveries are Hub-owned files. An artifact id or a Node path is never a delivery id. */
export type Delivery = {
  /** null: published outside an item (a user's own upload, a chat or scheduled run) */
  id: string; item_id: string | null; title: string; name: string; mime: string; size: number; created: number
  entrypoint: string | null; url: string; download_url: string; content_url: string; access_warning: string | null
  /** The owner's per-delivery switch: HTML runs its scripts in the isolated frame. Static otherwise. */
  interactive?: boolean
  /** The previewed HTML asks for scripts (a static preview of it may look empty). */
  scripts?: boolean
  /** The isolated frame's own URL (a capability), present only while interactive. */
  frame_url?: string | null
  /** Signed-in metadata: this user may switch the interactive preview. */
  can_manage?: boolean
  /** `local`: the configured address works only on the Hub's machine. */
  reachability?: "local" | "unverified"
  /** Authenticated metadata supplies the verified owner workspace; anonymous shares omit it. */
  workspace_id?: string
  /** Optional existing preview contract, especially for server-converted documents. */
  preview?: { type: "text"; lang: string; text: string; truncated: boolean; from?: string } | null
  /** A share may additionally report its deadline, so an already-open page can retire its preview. */
  expires_at?: number
}
export type DeliveryShare = { token: string; url: string; expires_at: number; revoked_at?: number | null }
export type DeliveryRoute = { id: string; shared: boolean }

/** Bind a standalone page before App loads /api/shell. Never substitute the browser's current workspace. */
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

export type DeliveryForm = "html" | "image" | "pdf" | "audio" | "video" | "office" | "text" | "binary"
const TEXT = /\.(md|markdown|mdx|txt|log|json|jsonl|csv|tsv|ya?ml|xml|toml|ini|sql|diff|patch|sh|bash|zsh|py|rb|go|rs|java|kt|swift|c|h|cc|cpp|hpp|cs|js|mjs|cjs|jsx|ts|tsx|css|scss|vue|svelte)$/i
/** The form of the document shown: a package's entry by its own name, a plain file by name and type. */
export function deliveryForm(d: Pick<Delivery, "name" | "mime" | "entrypoint">): DeliveryForm {
  const mime = d.entrypoint ? "" : d.mime.split(";")[0].toLowerCase(), name = d.entrypoint ?? d.name
  if (mime === "text/html" || /\.html?$/i.test(name)) return "html"
  if (mime.startsWith("image/") || /\.(png|jpe?g|gif|webp|avif|bmp|ico|svg)$/i.test(name)) return "image"
  if (mime === "application/pdf" || /\.pdf$/i.test(name)) return "pdf"
  if (mime.startsWith("video/") || /\.(mp4|m4v|webm|mov)$/i.test(name)) return "video"
  if (mime.startsWith("audio/") || /\.(mp3|wav|m4a|ogg|oga|flac)$/i.test(name)) return "audio"
  if (/\.(docx|xlsx|pptx)$/i.test(name)) return "office"
  if (mime.startsWith("text/") || mime === "application/json" || TEXT.test(name)) return "text"
  return "binary"
}

const LANGS: Record<string, string> = { md: "markdown", markdown: "markdown", mdx: "markdown", json: "json", jsonl: "json", csv: "csv", tsv: "tsv", xml: "xml",
  yml: "yaml", yaml: "yaml", toml: "toml", sql: "sql", diff: "diff", patch: "diff", sh: "bash", bash: "bash", zsh: "bash", py: "python", rb: "ruby", go: "go",
  rs: "rust", java: "java", kt: "kotlin", swift: "swift", c: "c", h: "c", cc: "cpp", cpp: "cpp", hpp: "cpp", cs: "csharp", js: "javascript", mjs: "javascript",
  cjs: "javascript", jsx: "jsx", ts: "typescript", tsx: "tsx", css: "css", scss: "scss", vue: "vue", svelte: "svelte" }
/** Markdown and code are told by their extension (a Markdown file may be stored as text/plain). */
export function deliveryTextLang(d: Pick<Delivery, "name" | "mime"> & { entrypoint?: string | null }): string {
  const name = d.entrypoint ?? d.name, ext = /\.([a-z0-9]+)$/i.exec(name)?.[1].toLowerCase()
  if (ext && LANGS[ext]) return LANGS[ext]
  return /markdown/i.test(d.mime) ? "markdown" : "text"
}

/** A relative reference in a package document (a Markdown image), resolved inside the package only:
 *  against the entry's resource URL, keeping its workspace query, and never above the package root. */
export function packageUrl(ref: string, entryUrl: string, entrypoint: string | null): string | null {
  if (!ref || /^[a-z][a-z0-9+.-]*:/i.test(ref) || ref.startsWith("//") || ref.startsWith("#")) return null
  try {
    const entry = new URL(entryUrl), depth = (entrypoint ?? "").split("/").length - 1
    const root = entry.pathname.split("/").slice(0, entry.pathname.split("/").length - 1 - depth).join("/") + "/"
    const u = new URL(ref, entry)
    if (u.origin !== entry.origin || !u.pathname.startsWith(root)) return null
    u.search = entry.search; u.hash = ""
    return u.href
  } catch { return null }
}

/** New artifact payloads add delivery_id; older local paths stay in the existing workspace preview. */
export function outputDeliveryId(o: readonly unknown[]): string | null {
  if (typeof o[6] === "string" && o[6]) return o[6]
  if (typeof o[2] !== "string") return null
  return deliveryRoute(o[2])?.shared === false ? deliveryRoute(o[2])!.id : null
}
