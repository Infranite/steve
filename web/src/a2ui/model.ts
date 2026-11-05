/* A2UI v0.9.1 (docs/ST-46-a2ui.md): a ```a2ui block in a reply holds server-to-client messages (JSON Lines, pretty-printed
 * objects one after another, or one array). They are applied in order into surfaces — a flat component list by id and a
 * data model — that the renderer draws. Nothing here touches the DOM: the parser, bindings and functions are plain data
 * so they can be tested on their own. Everything an agent can get wrong becomes a `Problem` with a JSON Pointer, never a throw. */
import { t } from "../i18n/index.ts"
import { date } from "../i18n/format.ts"

export const BASIC = "https://a2ui.org/specification/v0_9/catalogs/basic/catalog.json"
/** the basic catalog plus `Chart` */
export const STEVE = "steve:catalog/v1"
const VERSIONS = new Set(["v0.9", "v0.9.1"])
export const LIMITS = { bytes: 64 * 1024, components: 300, depth: 40, items: 500 }

export type Comp = { id: string; component: string; [k: string]: unknown }
export type Surface = { id: string; catalog: string; comps: Map<string, Comp>; data: unknown; theme?: Record<string, unknown> }
export type Problem = { path: string; message: string; surfaceId?: string }
export type Doc = { surfaces: Surface[]; problems: Problem[]; fatal?: string }

/** the top-level JSON values in `src`, in order: one per line, several pretty-printed, or the items of one array */
export function splitJson(src: string): { text: string; at: number }[] {
  const out: { text: string; at: number }[] = []
  let depth = 0, str = false, esc = false, start = -1
  for (let i = 0; i < src.length; i++) {
    const c = src[i]
    if (str) { if (esc) esc = false; else if (c === "\\") esc = true; else if (c === "\"") str = false; continue }
    if (c === "\"") { str = true; if (depth === 0 && start < 0) start = i; continue }
    /* text before a value is kept as its own part, so it fails to parse visibly instead of vanishing */
    if (c === "{" || c === "[") { if (depth === 0) { if (start >= 0) out.push({ text: src.slice(start, i).trim(), at: start }); start = i } depth++ }
    else if (c === "}" || c === "]") { depth--; if (depth === 0 && start >= 0) { out.push({ text: src.slice(start, i + 1), at: start }); start = -1 } if (depth < 0) depth = 0 }
    else if (depth === 0 && !/\s|,/.test(c) && start < 0) { start = i }
  }
  if (start >= 0) out.push({ text: src.slice(start), at: start })
  return out
}

const isObj = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v)

/** reads a block into surfaces; `fatal` when nothing could be drawn at all */
export function parseBlock(src: string): Doc {
  const problems: Problem[] = [], surfaces = new Map<string, Surface>()
  if (src.length > LIMITS.bytes) return { surfaces: [], problems, fatal: t("misc:a2ui.model.tooLarge", { kb: LIMITS.bytes / 1024 }) }
  const msgs: unknown[] = []
  for (const part of splitJson(src)) {
    let v: unknown
    try { v = JSON.parse(part.text) } catch (e) { problems.push({ path: `@${part.at}`, message: t("misc:a2ui.model.badJson", { error: (e as Error).message }) }); continue }
    if (Array.isArray(v)) msgs.push(...v); else msgs.push(v)
  }
  if (!msgs.length) return { surfaces: [], problems, fatal: problems.length ? t("misc:a2ui.model.notJson") : t("misc:a2ui.model.empty") }
  msgs.forEach((m, i) => apply(m, `/${i}`, surfaces, problems))
  const list = [...surfaces.values()]
  for (const s of list) if (!s.comps.has("root")) problems.push({ path: `/${s.id}`, surfaceId: s.id, message: t("misc:a2ui.model.noRoot") })
  const drawable = list.filter((s) => s.comps.has("root"))
  return { surfaces: drawable, problems, fatal: drawable.length ? undefined : list.length ? t("misc:a2ui.model.noRootSurface") : t("misc:a2ui.model.nothing") }
}

function apply(m: unknown, at: string, surfaces: Map<string, Surface>, problems: Problem[]) {
  if (!isObj(m)) { problems.push({ path: at, message: t("misc:a2ui.model.notObject") }); return }
  if (m.version !== undefined && !VERSIONS.has(String(m.version))) { problems.push({ path: `${at}/version`, message: t("misc:a2ui.model.badVersion", { version: String(m.version) }) }); return }
  const op = ["createSurface", "updateComponents", "updateDataModel", "deleteSurface"].find((k) => k in m)
  if (!op) { problems.push({ path: at, message: t("misc:a2ui.model.noOp") }); return }
  const body = m[op], p = `${at}/${op}`
  if (!isObj(body) || typeof body.surfaceId !== "string" || !body.surfaceId) { problems.push({ path: `${p}/surfaceId`, message: t("misc:a2ui.model.noSurfaceId") }); return }
  const id = body.surfaceId
  const get = () => {
    let s = surfaces.get(id)
    /* a forgotten createSurface is the most common slip: draw it with Steve's catalog rather than nothing */
    if (!s) { problems.push({ path: p, surfaceId: id, message: t("misc:a2ui.model.notCreated", { id }) }); s = { id, catalog: STEVE, comps: new Map(), data: {} }; surfaces.set(id, s) }
    return s
  }
  if (op === "createSurface") {
    if (surfaces.has(id)) { problems.push({ path: p, surfaceId: id, message: t("misc:a2ui.model.exists", { id }) }); return }
    let catalog = typeof body.catalogId === "string" ? body.catalogId : ""
    if (catalog !== BASIC && catalog !== STEVE) { problems.push({ path: `${p}/catalogId`, surfaceId: id, message: t("misc:a2ui.model.unknownCatalog", { catalog: catalog || t("misc:a2ui.model.emptyCatalog") }) }); catalog = STEVE }
    surfaces.set(id, { id, catalog, comps: new Map(), data: {}, theme: isObj(body.theme) ? body.theme : undefined })
  } else if (op === "updateComponents") {
    const s = get()
    if (!Array.isArray(body.components)) { problems.push({ path: `${p}/components`, surfaceId: id, message: t("misc:a2ui.model.notArray") }); return }
    body.components.forEach((c, j) => {
      const cp = `${p}/components/${j}`
      if (!isObj(c) || typeof c.id !== "string" || typeof c.component !== "string") { problems.push({ path: cp, surfaceId: id, message: t("misc:a2ui.model.badComponent") }); return }
      if (!s.comps.has(c.id) && s.comps.size >= LIMITS.components) { problems.push({ path: cp, surfaceId: id, message: t("misc:a2ui.model.tooMany", { n: LIMITS.components }) }); return }
      s.comps.set(c.id, c as Comp)
    })
  } else if (op === "updateDataModel") {
    const s = get(), path = typeof body.path === "string" && body.path ? body.path : "/"
    s.data = setPointer(s.data, path, "value" in body ? body.value : undefined, !("value" in body))
  } else {
    if (!surfaces.delete(id)) problems.push({ path: p, surfaceId: id, message: t("misc:a2ui.model.missing", { id }) })
  }
}

/* ───────── JSON Pointer ───────── */
const unesc = (t: string) => t.replace(/~1/g, "/").replace(/~0/g, "~")
export const tokens = (ptr: string) => (ptr === "/" || ptr === "" ? [] : ptr.replace(/^\//, "").split("/").map(unesc))
export function getPointer(data: unknown, ptr: string): unknown {
  let cur = data
  for (const t of tokens(ptr)) { if (cur == null || typeof cur !== "object") return undefined; cur = (cur as Record<string, unknown>)[t] }
  return cur
}
/** a copy of `data` with `ptr` set to `value` (missing objects are made); `remove` deletes the key (an array keeps its length) */
export function setPointer(data: unknown, ptr: string, value: unknown, remove = false): unknown {
  const ts = tokens(ptr)
  if (!ts.length) return remove ? {} : value
  const put = (cur: unknown, i: number): unknown => {
    const t = ts[i], last = i === ts.length - 1
    const box: Record<string, unknown> | unknown[] = Array.isArray(cur) ? [...cur] : isObj(cur) ? { ...cur } : /^\d+$/.test(t) ? [] : {}
    const rec = box as Record<string, unknown>
    if (last) { if (!remove) rec[t] = value; else if (Array.isArray(box)) rec[t] = undefined; else delete rec[t] }
    else rec[t] = put(rec[t], i + 1)
    return box
  }
  return put(data, 0)
}
/** a path as written in a component, made absolute inside a template's item (`base`) */
export const absolute = (path: string, base: string) => (path.startsWith("/") ? path : `${base.replace(/\/$/, "")}/${path}`)

/* ───────── dynamic values and functions ───────── */
export type Scope = { data: unknown; base: string }
export const isBinding = (v: unknown): v is { path: string } => isObj(v) && typeof v.path === "string" && !("call" in v)
const isCall = (v: unknown): v is { call: string; args?: Record<string, unknown> } => isObj(v) && typeof v.call === "string"

/** a property's value: a literal, a `{path}` binding or a `{call}` function */
export function resolve(v: unknown, s: Scope): unknown {
  if (isBinding(v)) return getPointer(s.data, absolute(v.path, s.base))
  if (isCall(v)) return call(v.call, v.args ?? {}, s)
  return v
}
export const str = (v: unknown): string => v == null ? "" : typeof v === "string" ? v : typeof v === "number" || typeof v === "boolean" ? String(v) : JSON.stringify(v)
const num = (v: unknown) => (typeof v === "number" ? v : typeof v === "string" && v.trim() !== "" ? Number(v) : NaN)

/** a function of the basic catalog; `openUrl` returns the url it would open (the renderer opens it) */
export function call(name: string, raw: Record<string, unknown>, s: Scope): unknown {
  const a = (k: string) => resolve(raw[k], s)
  switch (name) {
    case "required": { const v = a("value"); return !(v == null || v === "" || (Array.isArray(v) && !v.length)) }
    case "regex": {
      const v = str(a("value")), p = str(raw.pattern)
      if (p.length > 200 || v.length > 2000) return false
      try { return new RegExp(p).test(v) } catch { return false }
    }
    case "length": { const n = str(a("value")).length, lo = num(a("min")), hi = num(a("max")); return !(n < lo) && !(n > hi) }
    case "numeric": { const n = num(a("value")), lo = num(a("min")), hi = num(a("max")); return Number.isFinite(n) && !(n < lo) && !(n > hi) }
    case "email": return /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(str(a("value")))
    case "formatString": return formatString(str(raw.value), s)
    case "formatNumber": { const n = num(a("value")); if (!Number.isFinite(n)) return ""; const d = num(a("decimals")); return n.toLocaleString("zh-CN", { useGrouping: a("grouping") !== false, ...(Number.isFinite(d) ? { minimumFractionDigits: d, maximumFractionDigits: d } : {}) }) }
    case "formatCurrency": {
      const n = num(a("value")); if (!Number.isFinite(n)) return ""
      const d = num(a("decimals"))
      try { return n.toLocaleString("zh-CN", { style: "currency", currency: str(a("currency")) || "CNY", useGrouping: a("grouping") !== false, ...(Number.isFinite(d) ? { minimumFractionDigits: d, maximumFractionDigits: d } : {}) }) } catch { return String(n) }
    }
    case "formatDate": return formatDate(a("value"), str(a("format")))
    case "pluralize": {
      const n = num(a("value"))
      let cat: string
      try { cat = new Intl.PluralRules("zh-CN").select(n) } catch { cat = "other" }
      const pick = (n === 0 && raw.zero !== undefined ? a("zero") : raw[cat] !== undefined ? a(cat) : a("other"))
      return str(pick).replace(/#/g, String(n))
    }
    case "openUrl": return str(a("url"))
    case "and": return Array.isArray(raw.values) && raw.values.every((x) => !!resolve(x, s))
    case "or": return Array.isArray(raw.values) && raw.values.some((x) => !!resolve(x, s))
    case "not": return !a("value")
    default: return undefined
  }
}

/** `${/path}` and `${relative}` from the data model; `\${` stays literal */
export function formatString(t: string, s: Scope): string {
  return t.replace(/(\\?)\$\{([^}]*)\}/g, (all, bs: string, expr: string) => {
    if (bs) return all.slice(1)
    const e = expr.trim()
    const fn = /^(\w+)\((.*)\)$/.exec(e)
    if (fn) {
      /* `${formatDate(value:${/d}, format:'MM-dd')}` was already split at the inner `}`: keep it simple, named literal args only */
      const args: Record<string, unknown> = {}
      for (const m of fn[2].matchAll(/(\w+)\s*:\s*('([^']*)'|"([^"]*)"|([^,]+))/g)) {
        const v = m[3] ?? m[4] ?? m[5]?.trim()
        args[m[1]] = v && /^\//.test(v) ? { path: v } : v !== undefined && /^-?\d+(\.\d+)?$/.test(v) ? Number(v) : v
      }
      return str(call(fn[1], args, s))
    }
    return str(getPointer(s.data, absolute(e, s.base)))
  })
}

/** yyyy MM dd HH mm ss tokens; anything else prints as is */
export function formatDate(v: unknown, f: string): string {
  const d = typeof v === "number" ? new Date(v) : typeof v === "string" ? new Date(v) : null
  if (!d || Number.isNaN(d.getTime())) return str(v)
  const p = (n: number) => String(n).padStart(2, "0")
  if (!f) return date(d, { year: "numeric", month: "numeric", day: "numeric", hour: "numeric", minute: "numeric", second: "numeric" })
  return f.replace(/yyyy|MM|dd|HH|mm|ss/g, (t) => t === "yyyy" ? String(d.getFullYear()) : t === "MM" ? p(d.getMonth() + 1) : t === "dd" ? p(d.getDate()) : t === "HH" ? p(d.getHours()) : t === "mm" ? p(d.getMinutes()) : p(d.getSeconds()))
}

/** a component's `checks` that fail, as their messages */
export function failedChecks(c: Comp, s: Scope): string[] {
  if (!Array.isArray(c.checks)) return []
  return c.checks.filter((k) => isObj(k) && !resolve(k.condition, s)).map((k) => str((k as Record<string, unknown>).message) || t("misc:a2ui.model.checkFailed"))
}

/** an event's `context` with every binding read (what the action message carries) */
export function eventContext(ctx: unknown, s: Scope): Record<string, unknown> {
  if (!isObj(ctx)) return {}
  return Object.fromEntries(Object.entries(ctx).map(([k, v]) => [k, resolve(v, s)]))
}

/** the children of a container: static ids, or one template instance per item of a data list (with its item path) */
export function childrenOf(v: unknown, s: Scope): { id: string; base: string; key: string }[] {
  if (Array.isArray(v)) return v.filter((x): x is string => typeof x === "string").map((id) => ({ id, base: s.base, key: id }))
  if (isObj(v) && typeof v.componentId === "string" && typeof v.path === "string") {
    const p = absolute(v.path, s.base), list = getPointer(s.data, p)
    const keys = Array.isArray(list) ? list.map((_, i) => String(i)) : isObj(list) ? Object.keys(list) : []
    return keys.slice(0, LIMITS.items).map((k) => ({ id: v.componentId as string, base: `${p}/${k}`, key: `${v.componentId}@${p}/${k}` }))
  }
  return []
}

/** a short stable id for a block's text (FNV-1a), so a submitted action stays tied to the block it came from */
export function blockKey(src: string): string {
  let h = 0x811c9dc5
  for (let i = 0; i < src.length; i++) { h ^= src.charCodeAt(i); h = Math.imul(h, 0x01000193) }
  return (h >>> 0).toString(36)
}

/** the key the Hub dedupes on: a surface of a block is submitted once (whichever of its buttons), then it is a record */
export const actionKey = (block: string, surface: string) => `${block}:${surface}`

/** rows of a Chart: the data (a list of objects), only the last `last` rows when it is set */
export function chartRows(data: unknown, last: unknown): Record<string, unknown>[] {
  const rows = Array.isArray(data) ? data.filter(isObj) : []
  const n = num(Array.isArray(last) ? last[0] : last)
  return Number.isFinite(n) && n > 0 ? rows.slice(-Math.floor(n)) : rows
}
