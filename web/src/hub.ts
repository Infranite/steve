/* Which Hub this page talks to (A3). Served by a Hub, the page talks to it (same origin, "" here); a web client
 * served on its own asks for the Hub's address once and keeps it. Every request goes through `hubUrl` and carries
 * the client's cookie (`credentials: "include"`): a Hub on the same host (another port) is the same site, so its
 * SameSite=Lax cookie goes along; a Hub on another host needs HTTPS (the Hub then sets SameSite=None). */

const KEY = "steve.hub"

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
    return u.origin
  } catch { return null }
}

/** asks `base` who it is; throws with a reason a person can act on */
export async function hello(base: string): Promise<Hello> {
  const r = await fetch(base + "/auth/hello", { credentials: "include", cache: "no-store", signal: AbortSignal.timeout(8000) })
  const j = await r.json().catch(() => null)
  if (!r.ok || !j || j.name !== "steve") throw new Error("not-steve")
  return j as Hello
}

/** keeps `base` as this page's Hub ("" or null: the one that served it) */
export const setHub = (base: string | null) => {
  try { if (base) localStorage.setItem(KEY, base); else localStorage.removeItem(KEY) } catch { /* private mode */ }
}
