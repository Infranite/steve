/* Which Hub this page talks to (A3), plus saved multi-Hub switching, `steve://pair` URI parsing, and the optional
 * native shell bridge (`window.__STEVE_NATIVE__`) injected by SteveMac / SteveIOS.
 *
 * Served by a Hub, the page talks to it (same origin, "" here); a web client served on its own asks for the Hub's
 * address once and keeps it, while allowing switching among saved Hubs at any time. A Hub on another host needs HTTPS:
 * cross-site cookies require `SameSite=None; Secure`, which browsers refuse over plain HTTP. */

const KEY = "steve.hub"
const HUBS_KEY = "steve.hubs"

export type SavedHub = {
  /** canonical origin, e.g. `https://steve.example.com` or `http://192.168.1.10:8440` */
  url: string
  /** optional display label */
  name?: string
  /** last seen Hub version */
  version?: string
  /** epoch ms when last selected or verified */
  lastUsed: number
}

export type PairUriPayload = {
  code: string
  name?: string
  publicUrl?: string
  lanUrls: string[]
}

export type NativeBridge = {
  platform: "macos" | "ios"
  activeHub?: { id: string; name: string; endpoint: string; route?: string }
  openHubSwitcher?: () => void
  openPairSheet?: () => void
  openQRScanner?: () => void
}

declare global {
  interface Window {
    __STEVE_NATIVE__?: NativeBridge
  }
}

/** returns the native shell bridge when running inside SteveMac or SteveIOS */
export const nativeBridge = (): NativeBridge | null => {
  try {
    return typeof window !== "undefined" && window.__STEVE_NATIVE__ ? window.__STEVE_NATIVE__ : null
  } catch {
    return null
  }
}

/** the Hub's origin, or "" for the one that served this page */
export const hubBase = (): string => {
  try { return localStorage.getItem(KEY) ?? "" } catch { return "" }
}

/** a Hub path (`/api/…`, `/auth/…`, `/healthz`) as the URL to call; anything else is left as it is */
export const hubUrl = (path: string): string => (path.startsWith("/") && !path.startsWith("//") ? hubBase() + path : path)

/** what the Hub says of itself (`GET /auth/hello`) */
export type Hello = { name: string; version: string; api: number; login: { token: boolean; oidc: boolean } }

/** the API generation this page speaks */
export const API = 1

/** `url` without a trailing slash, with `http://` when no scheme was typed; null when it is no URL */
export const normalize = (url: string): string | null => {
  const s = url.trim().replace(/\/+$/, "")
  if (!s) return null
  try {
    const u = new URL(/^https?:\/\//i.test(s) ? s : `http://${s}`)
    if (u.protocol !== "http:" && u.protocol !== "https:") return null
    return u.origin
  } catch { return null }
}

/** true when `targetOrigin` is on a different hostname than `pageOrigin` and uses plain `http:` (so browser cookies cannot stick) */
export const needsHttpsForCrossHost = (targetOrigin: string, pageOrigin?: string): boolean => {
  try {
    const target = new URL(targetOrigin)
    if (target.protocol !== "http:") return false
    const here = new URL(pageOrigin ?? (typeof location !== "undefined" ? location.origin : targetOrigin))
    return target.hostname.toLowerCase() !== here.hostname.toLowerCase()
  } catch {
    return false
  }
}

/** asks `base` who it is; throws with a reason a person can act on */
export async function hello(base: string): Promise<Hello> {
  /* not followed: a web client on its own port sends /auth/… on to its Hub, and must not pass for one */
  const r = await fetch(base + "/auth/hello", { credentials: "include", cache: "no-store", redirect: "manual", signal: AbortSignal.timeout(8000) })
  const j = await r.json().catch(() => null)
  if (!r.ok || !j || j.name !== "steve") throw new Error("not-steve")
  return j as Hello
}

/** reads the saved Hubs list, most recently used first */
export function savedHubs(): SavedHub[] {
  try {
    const raw = localStorage.getItem(HUBS_KEY)
    if (!raw) return []
    const parsed = JSON.parse(raw)
    if (!Array.isArray(parsed)) return []
    const seen = new Set<string>()
    const out: SavedHub[] = []
    for (const item of parsed) {
      if (!item || typeof item.url !== "string") continue
      const u = normalize(item.url)
      if (!u || seen.has(u)) continue
      seen.add(u)
      out.push({
        url: u,
        ...(typeof item.name === "string" && item.name.trim() ? { name: item.name.trim() } : {}),
        ...(typeof item.version === "string" && item.version ? { version: item.version } : {}),
        lastUsed: typeof item.lastUsed === "number" ? item.lastUsed : 0,
      })
    }
    return out.sort((a, b) => b.lastUsed - a.lastUsed)
  } catch {
    return []
  }
}

/** records or updates a Hub in the saved list */
export function rememberHub(url: string, meta: { name?: string; version?: string; now?: number } = {}): SavedHub[] {
  const canonical = normalize(url)
  if (!canonical) return savedHubs()
  const existing = savedHubs()
  const prev = existing.find((h) => h.url === canonical)
  const entry: SavedHub = {
    url: canonical,
    ...(meta.name?.trim() ? { name: meta.name.trim() } : prev?.name ? { name: prev.name } : {}),
    ...(meta.version ? { version: meta.version } : prev?.version ? { version: prev.version } : {}),
    lastUsed: meta.now ?? Date.now(),
  }
  const next = [entry, ...existing.filter((h) => h.url !== canonical)].slice(0, 20)
  try { localStorage.setItem(HUBS_KEY, JSON.stringify(next)) } catch { /* private mode */ }
  return next
}

/** removes a Hub from the saved list */
export function forgetHub(url: string): SavedHub[] {
  const canonical = normalize(url) ?? url
  const next = savedHubs().filter((h) => h.url !== canonical)
  try {
    if (next.length) localStorage.setItem(HUBS_KEY, JSON.stringify(next))
    else localStorage.removeItem(HUBS_KEY)
  } catch { /* private mode */ }
  return next
}

/** keeps `base` as this page's Hub ("" or null: the one that served it), and remembers non-empty origins */
export const setHub = (base: string | null, meta?: { name?: string; version?: string }) => {
  try {
    if (base) {
      localStorage.setItem(KEY, base)
      rememberHub(base, meta)
    } else {
      localStorage.removeItem(KEY)
    }
  } catch { /* private mode */ }
}

/** parses a `steve://pair?v=1&code=stp_…&pub=…&lan=…` URI, returning null when malformed */
export function parsePairUri(uri: string): PairUriPayload | null {
  const trimmed = uri.trim()
  if (!/^steve:\/\/pair(?:\?|$)/i.test(trimmed)) return null
  try {
    const qIndex = trimmed.indexOf("?")
    const params = new URLSearchParams(qIndex >= 0 ? trimmed.slice(qIndex + 1) : "")
    const code = (params.get("code") ?? "").trim()
    if (!code.startsWith("stp_") || code.length < 12) return null
    const name = (params.get("name") ?? "").trim() || undefined
    const pubRaw = params.get("pub")
    const publicUrl = pubRaw ? normalize(pubRaw) ?? undefined : undefined
    const lanUrls: string[] = []
    for (const raw of params.getAll("lan")) {
      for (const piece of raw.split(",")) {
        const u = normalize(piece)
        if (u && u !== publicUrl && !lanUrls.includes(u)) lanUrls.push(u)
      }
    }
    return { code, ...(name ? { name } : {}), ...(publicUrl ? { publicUrl } : {}), lanUrls }
  } catch {
    return null
  }
}

/** selects the preferred initial endpoint from a pairing payload: public network by default when present, else LAN */
export function preferredPairEndpoint(p: Pick<PairUriPayload, "publicUrl" | "lanUrls">): { url: string | null; mode: "public" | "lan" | "none" } {
  if (p.publicUrl) return { url: p.publicUrl, mode: "public" }
  if (p.lanUrls.length > 0) return { url: p.lanUrls[0], mode: "lan" }
  return { url: null, mode: "none" }
}
