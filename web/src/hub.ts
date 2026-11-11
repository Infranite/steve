/* Which Hub this page talks to (A3), the Hubs a person switches among, pairing links, and the bridge to a native
 * shell (`window.__STEVE_NATIVE__`, set by SteveMac / SteveIOS).
 *
 * Served by a Hub, the page talks to it (same origin, "" here); a web client served on its own asks for the Hub's
 * address once and keeps it. The list of Hubs, and everything done with it, is the same in a browser and inside the
 * apps (`hubDirectory`): a browser keeps the list in localStorage, a shell keeps it and answers through the bridge.
 *
 * A browser page gets to another Hub in one of two ways (`reach`). Where a cookie can stick (an https Hub, or the
 * same host on another port) it calls that Hub across origins. Otherwise (plain http on another host, or an http Hub
 * from an https page) it goes to that Hub's own page, and takes the list along in the fragment of the address. */

const KEY = "steve.hub"
const HUBS_KEY = "steve.hubs"
const OFFER_KEY = "steve.hubs.offer"

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

/** what a pairing link holds: the single-use code, and the addresses of the Hub that issued it */
export type PairLink = {
  code: string
  name?: string
  publicUrl?: string
  lanUrls: string[]
}

/** a Hub as a native shell lists it */
export type NativeHub = {
  id: string
  name: string
  /** the endpoint in use, else the first known */
  address: string
  current: boolean
  /** a credential is in the Keychain */
  signedIn: boolean
  /** loopback | lan | tailnet | public, when connected */
  route?: string
  version?: string
  /** false for the local Hub the shell found by itself */
  removable: boolean
}

/** the native shell's side: Keychain, gateway, camera. Every screen about Hubs and pairing is this page's. */
export type NativeBridge = {
  v: 2
  platform: "macos" | "ios"
  /** the operations this shell answers */
  can: string[]
  /** asks the shell; rejects with an Error whose message can be shown */
  call(op: string, args?: Record<string, unknown>): Promise<any>
}

declare global {
  interface Window {
    __STEVE_NATIVE__?: NativeBridge
  }
}

/** the native shell's bridge inside SteveMac or SteveIOS, else null */
export const nativeBridge = (): NativeBridge | null => {
  try {
    const b = typeof window !== "undefined" ? window.__STEVE_NATIVE__ : undefined
    return b && b.v === 2 && typeof b.call === "function" ? b : null
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

/** the address of the Hub this page talks to */
export const currentHub = (): string => hubBase() || (typeof location !== "undefined" ? location.origin : "")

/** what the Hub says of itself (`GET /auth/hello`) */
export type Hello = { name: string; version: string; api: number; login: { token: boolean; oidc: boolean } }

/** the API generation this page speaks */
export const API = 1

/** why a Hub could not be used, for the page to say in its own words */
export class HubError extends Error {
  code: "bad-address" | "not-steve" | "api-mismatch"
  version?: string
  constructor(code: HubError["code"], version?: string) { super(code); this.code = code; this.version = version }
}

/** the origin of `url`, with `http://` when no scheme was typed; null when it is no http(s) URL */
export const normalize = (url: string): string | null => {
  const s = url.trim()
  if (!s) return null
  /* another scheme is no Hub's address; with `http://` put before it, it would read as a host named after the scheme */
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(s) && !/^https?:\/\//i.test(s)) return null
  try {
    const u = new URL(/^https?:\/\//i.test(s) ? s : `http://${s}`)
    if (u.protocol !== "http:" && u.protocol !== "https:") return null
    return u.origin
  } catch { return null }
}

/** asks `base` who it is; throws with a reason a person can act on */
export async function hello(base: string, timeout = 8000): Promise<Hello> {
  /* not followed: a web client on its own port sends /auth/… on to its Hub, and must not pass for one */
  const r = await fetch(base + "/auth/hello", { credentials: "include", cache: "no-store", redirect: "manual", signal: AbortSignal.timeout(timeout) })
  const j = await r.json().catch(() => null)
  if (!r.ok || !j || j.name !== "steve") throw new Error("not-steve")
  return j as Hello
}

/** `hello`, as the answer to "can this page use that Hub" */
async function check(base: string): Promise<Hello> {
  let h: Hello
  try { h = await hello(base) } catch { throw new HubError("not-steve") }
  if (h.api !== API) throw new HubError("api-mismatch", h.version)
  return h
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
  const next = [entry, ...existing.filter((h) => h.url !== canonical)].sort((a, b) => b.lastUsed - a.lastUsed).slice(0, 20)
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

/* ───────── reaching a Hub from a browser page ───────── */

/** `call`: this page can hold a session with the Hub at `target`. `open`: it cannot, the Hub's own page can. */
export type Reach = "call" | "open"

/** How the page at `page` gets to the Hub at `target`. A session is a cookie the Hub sets: across hosts a browser
 * keeps one only as `SameSite=None; Secure`, so only for an https Hub; and an https page may not call plain http. */
export const reach = (target: string, page?: string): Reach => {
  try {
    const t = new URL(target)
    const p = new URL(page ?? location.origin)
    if (t.origin === p.origin) return "call"
    if (t.protocol === "https:") return "call"
    return p.protocol === "http:" && t.hostname.toLowerCase() === p.hostname.toLowerCase() ? "call" : "open"
  } catch {
    return "open"
  }
}

/** a Hub named in another page's list */
export type CarriedHub = { url: string; name?: string }

/** The address of the Hub at `target` as a page to go to, with `hubs` for that page to offer. A Hub whose pages are
 * on another port sends the browser on to it; the fragment goes along, and never reaches a server. */
export const openUrl = (target: string, hubs: CarriedHub[] = []): string =>
  `${target}/#/` + (hubs.length ? `?hubs=${encodeURIComponent(JSON.stringify(hubs.slice(0, 20).map((h) => (h.name ? [h.url, h.name] : [h.url]))))}` : "")

/** reads the list `openUrl` wrote; whatever else it is given, an empty list */
export function parseCarried(raw: string): CarriedHub[] {
  try {
    const xs = JSON.parse(raw)
    if (!Array.isArray(xs)) return []
    const out: CarriedHub[] = []
    for (const x of xs.slice(0, 20)) {
      if (!Array.isArray(x) || typeof x[0] !== "string") continue
      const url = normalize(x[0])
      if (!url || out.some((h) => h.url === url)) continue
      const name = typeof x[1] === "string" ? x[1].trim().slice(0, 60) : ""
      out.push({ url, ...(name ? { name } : {}) })
    }
    return out
  } catch {
    return []
  }
}

/* ───────── pairing links ───────── */

/** Reads a pairing link: `<a Hub's page>/#/pair?v=1&code=stp_…&name=…&pub=…&lan=…`, or the same query after
 * `steve://pair?`. Null when the text is neither. The link's own host is where a page is, not an address of the Hub. */
export function parsePairLink(text: string): PairLink | null {
  const s = text.trim()
  const m = /^steve:\/\/pair\/?(?:\?(.*))?$/i.exec(s) ?? /^https?:\/\/[^#\s]+#\/pair(?:\?(.*))?$/i.exec(s)
  if (!m) return null
  try {
    const params = new URLSearchParams(m[1] ?? "")
    const code = (params.get("code") ?? "").trim()
    if (!code.startsWith("stp_") || code.length < 12) return null
    const name = (params.get("name") ?? "").trim().slice(0, 60) || undefined
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

/** the query of a pairing link, as it stands after `#/pair?` and `steve://pair?` */
export function pairQuery(p: PairLink): string {
  const q = new URLSearchParams({ v: "1", code: p.code })
  if (p.name) q.set("name", p.name)
  if (p.publicUrl) q.set("pub", p.publicUrl)
  if (p.lanUrls.length) q.set("lan", p.lanUrls.join(","))
  return q.toString().replace(/\+/g, "%20")
}

/** the link that opens a pairing in the Steve app */
export const appPairLink = (p: PairLink): string => `steve://pair?${pairQuery(p)}`

/** the Hub's addresses in a pairing link, the public one first */
export const pairAddresses = (p: Pick<PairLink, "publicUrl" | "lanUrls">): string[] => [...(p.publicUrl ? [p.publicUrl] : []), ...p.lanUrls]

/** What the page at `page` does with a pairing: the addresses it can claim the code at itself (its own host first:
 * the browser got here, so it gets there), else the address of the link on a page that can. */
export function pairPlan(p: PairLink, page?: string): { call: string[]; open: string | null } {
  const here = page ?? location.origin
  const all = pairAddresses(p)
  const host = (u: string) => { try { return new URL(u).hostname.toLowerCase() } catch { return "" } }
  const call = all.filter((u) => reach(u, here) === "call").sort((a, b) => Number(host(b) === host(here)) - Number(host(a) === host(here)))
  return { call, open: call.length || !all.length ? null : `${all[0]}/#/pair?${pairQuery(p)}` }
}

/** who this page is, as the Hub lists its clients */
export const clientInfo = (version: string): { kind: string; name: string; platform: string; version: string } => {
  const b = nativeBridge()
  const ua = navigator.userAgent
  const name = b?.platform === "macos" ? "Steve Mac" : b?.platform === "ios" ? "Steve iOS" : /Edg\//.test(ua) ? "Edge" : /Chrome\//.test(ua) ? "Chrome" : /Firefox\//.test(ua) ? "Firefox" : /Safari\//.test(ua) ? "Safari" : "Browser"
  return { kind: b?.platform === "macos" ? "mac" : b?.platform === "ios" ? "ios" : "web", name, platform: (navigator as any).userAgentData?.platform ?? navigator.platform ?? "", version }
}

/** Claims a pairing code for this page: asks the Hub's addresses this page can call, signs in at the first that
 * answers (the Hub keeps the credential in an HttpOnly cookie), and makes it this page's Hub. Throws a `HubError`,
 * or an Error with the Hub's own words (an expired or used code). */
export async function claimPair(p: PairLink, version: string): Promise<string> {
  let at = "", mismatch: Hello | null = null
  for (const base of pairPlan(p).call) {
    try {
      const h = await hello(base, 4000)
      if (h.api !== API) { mismatch = h; continue }
      at = base; break
    } catch { /* the next address */ }
  }
  if (!at) throw mismatch ? new HubError("api-mismatch", mismatch.version) : new HubError("not-steve")
  const r = await fetch(at + "/auth/pair", { method: "POST", headers: { "content-type": "application/json" }, credentials: "include", body: JSON.stringify({ code: p.code, cookie: true, client: clientInfo(version) }) })
  if (!r.ok) { const j = await r.json().catch(() => ({})); throw new Error(typeof j.error === "string" ? j.error : `HTTP ${r.status}`) }
  setHub(at === location.origin ? null : at, { name: p.name })
  return at
}

/* ───────── what the address the page was opened with asks for ───────── */

/** A pairing link this page was opened with (or that was given to its add field), waiting for the person's yes.
 * `link` is null when the address said "pair" and what followed could not be read. */
export type PairRequest = { link: PairLink | null; text: string }

/** Reads the fragment of an address: a pairing (`#/pair?…`), or the list another Hub's page sent along
 * (`#/?hubs=…`). `rest` is the fragment to leave in the address bar. Null for any other. */
export function readArrival(hash: string, href = ""): { pair?: PairRequest; hubs?: CarriedHub[]; rest: string } | null {
  if (/^#\/pair(?:\?|$)/.test(hash)) {
    const text = href || `steve://pair${hash.slice("#/pair".length)}`
    return { pair: { link: parsePairLink(`steve://pair${hash.slice("#/pair".length)}`), text }, rest: "#/" }
  }
  if (hash.startsWith("#/?")) {
    const raw = new URLSearchParams(hash.slice(3)).get("hubs")
    if (raw != null) return { hubs: parseCarried(raw), rest: "#/" }
  }
  return null
}

let request: PairRequest | null = null
const waiting = new Set<() => void>()
const tell = () => waiting.forEach((f) => f())
/** the pairing waiting for an answer */
export const pairRequest = (): PairRequest | null => request
/** `f` is called when a pairing or an offer of Hubs arrives, and when one is answered */
export const onArrival = (f: () => void): (() => void) => { waiting.add(f); return () => { waiting.delete(f) } }
/** answered, one way or the other */
export const dropPairRequest = () => { request = null; tell() }

/** whether the Hub of a pairing link is the one this page talks to, or is on the host that served the page. When it
 * is neither, the person is told before they say yes: the link would point this page at a Hub they did not pick. */
export function pairIsHere(p: PairLink, page?: string, hub?: string): boolean {
  const here = page ?? location.origin
  const host = (u: string) => { try { return new URL(u).hostname.toLowerCase() } catch { return "" } }
  return pairAddresses(p).some((u) => u === (hub ?? currentHub()) || host(u) === host(here))
}

/** the Hubs another page's list named that are not in this page's: offered, and added only when the person says so
 * (a link someone made up must not put a Hub into the list unseen) */
export function hubsOffer(): CarriedHub[] {
  try {
    const known = new Set([currentHub(), ...savedHubs().map((h) => h.url)])
    return parseCarried(sessionStorage.getItem(OFFER_KEY) ?? "").filter((h) => !known.has(h.url))
  } catch {
    return []
  }
}
/** the person's answer to the offer */
export function answerOffer(add: boolean) {
  try {
    if (add) hubsOffer().forEach((h, n) => rememberHub(h.url, { name: h.name, now: Date.now() - 1 - n }))
    sessionStorage.removeItem(OFFER_KEY)
  } catch { /* private mode */ }
  tell()
}

/** Takes what the address asks for out of the address bar and out of history (a pairing code must not stay in
 * either), and keeps it for the page to act on. */
function arrive() {
  const a = readArrival(location.hash, location.href)
  if (!a) return
  history.replaceState(null, "", location.pathname + location.search + a.rest)
  if (a.pair) request = a.pair
  /* inside a shell the list is the shell's: nothing is offered into it from an address */
  if (a.hubs?.length && !nativeBridge()) { try { sessionStorage.setItem(OFFER_KEY, JSON.stringify(a.hubs.map((h) => (h.name ? [h.url, h.name] : [h.url])))) } catch { /* private mode */ } }
  tell()
}
if (typeof window !== "undefined" && typeof location !== "undefined" && typeof history !== "undefined") {
  arrive()
  window.addEventListener("hashchange", arrive)
}

/* ───────── the list of Hubs, wherever the page runs ───────── */

/** a Hub in the list */
export type HubEntry = {
  id: string
  name: string
  address: string
  current: boolean
  /** known inside a shell only: a credential is kept for it */
  signedIn?: boolean
  route?: string
  version?: string
  removable: boolean
}

export type HubDirectory = {
  /** the list is the native shell's */
  native: boolean
  list(): Promise<HubEntry[]>
  /** makes `h` the Hub in use; the page reloads or goes to that Hub's page */
  switchTo(h: HubEntry): Promise<void>
  /** an address, or a pairing link; the Hub becomes the one in use */
  add(input: string, name?: string): Promise<void>
  forget(h: HubEntry): Promise<void>
  /** the shell's camera, where it has one: a scanned pairing link is confirmed and claimed by the shell */
  scan?: () => Promise<void>
}

const hostOf = (u: string) => { try { return new URL(u).host } catch { return u } }

/** the saved list as entries, the Hub in use among them, in a steady order */
export function browserEntries(): HubEntry[] {
  const cur = currentHub()
  const saved = savedHubs()
  const all: SavedHub[] = !cur || saved.some((h) => h.url === cur) ? saved : [{ url: cur, lastUsed: 0 }, ...saved]
  return all
    .map((h) => ({ id: h.url, name: h.name || hostOf(h.url), address: h.url, current: h.url === cur, ...(h.version ? { version: h.version } : {}), removable: h.url !== cur }))
    .sort((a, b) => a.name.localeCompare(b.name) || a.address.localeCompare(b.address))
}

function browserDirectory(): HubDirectory {
  const go = async (base: string, name?: string) => {
    if (reach(base) === "open") {
      /* an http page may ask it first, so a wrong address is told here and not as a browser's error page */
      if (location.protocol === "http:") await check(base)
      rememberHub(base, { name })
      location.href = openUrl(base, browserEntries().map((h) => ({ url: h.address, ...(h.name !== hostOf(h.address) ? { name: h.name } : {}) })))
      return
    }
    const h = await check(base)
    setHub(base === location.origin ? null : base, { name, version: h.version })
    location.reload()
  }
  return {
    native: false,
    list: async () => browserEntries(),
    switchTo: (h) => go(h.address),
    add: async (input, name) => {
      const link = parsePairLink(input)
      if (link) {
        const plan = pairPlan(link)
        if (plan.open) { location.href = plan.open; return }
        request = { link, text: input.trim() }
        tell()
        return
      }
      const base = normalize(input)
      if (!base) throw new HubError("bad-address")
      await go(base, name?.trim() || undefined)
    },
    forget: async (h) => { forgetHub(h.address) },
  }
}

function nativeDirectory(b: NativeBridge): HubDirectory {
  return {
    native: true,
    list: async () => (((await b.call("hubs.list"))?.hubs ?? []) as NativeHub[]).map((h) => ({ ...h })),
    switchTo: async (h) => { await b.call("hubs.switch", { id: h.id }) },
    add: async (input, name) => {
      if (parsePairLink(input)) { await b.call("pair.claim", { link: input.trim() }); return }
      const address = normalize(input)
      if (!address) throw new HubError("bad-address")
      await b.call("hubs.add", { address, ...(name?.trim() ? { name: name.trim() } : {}) })
    },
    forget: async (h) => { await b.call("hubs.forget", { id: h.id }) },
    ...(b.can.includes("pair.scan") ? { scan: async () => { await b.call("pair.scan") } } : {}),
  }
}

/** the Hubs of where this page runs: a browser's own list, or the native shell's */
export function hubDirectory(): HubDirectory {
  const b = nativeBridge()
  return b ? nativeDirectory(b) : browserDirectory()
}
