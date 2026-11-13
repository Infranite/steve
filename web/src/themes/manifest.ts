/* Theme manifests: what a theme may change, how a manifest is checked, and how the chosen ones combine into CSS
 * variables. No DOM here, so node --test can load it (imports carry .ts for the same reason).
 *
 * A theme overrides a whitelist of Steve's design tokens (web/src/index.css), separately for light and dark:
 *
 *   { "id": "ocean", "name": { "zh-CN": "海洋", "en": "Ocean" }, "kind": "accent",
 *     "tokens": { "light": { "primary": "#0b6bc0", … }, "dark": { … } } }
 *
 * `kind` says which choice in Settings → Appearance it belongs to and which tokens it may set:
 *   accent  — primary, its text, the focus ring, the running/link blue and the first chart hue
 *   neutral — page, cards, muted fills, hairlines, the text greys, the sidebar and the light-mode shadows
 *   sans    — `fonts.sans` (headings follow); mono — `fonts.mono`
 *   radius  — `radius`, a scale on the corner radii (1 = standard)
 *   full    — a skin: any whitelisted token, fonts and radius, laid over the other choices
 * Token names are written without the leading "--". Values are colours (#hex, rgb(), hsl(), oklch(), oklab(), lab(),
 * lch(), transparent, or var(--another-whitelisted-token)), or for the shadow tokens a box-shadow list of those
 * colours. A manifest with anything else is refused as a whole and the interface keeps what it had. */

export type Mode = "light" | "dark"
export type Kind = "accent" | "neutral" | "sans" | "mono" | "radius" | "full"
export const KINDS: readonly Kind[] = ["accent", "neutral", "sans", "mono", "radius", "full"]
export type Tokens = Partial<Record<string, string>>
export type Manifest = {
  id: string
  name: Partial<Record<string, string>>
  kind: Kind
  tokens?: { light?: Tokens; dark?: Tokens }
  fonts?: { sans?: string; mono?: string }
  radius?: number
}

export const ACCENT_TOKENS = ["primary", "primary-foreground", "ring", "blue", "chart-1", "sidebar-ring"] as const
export const NEUTRAL_TOKENS = [
  "background", "foreground", "card", "card-foreground", "popover", "popover-foreground", "secondary", "secondary-foreground",
  "muted", "muted-foreground", "accent", "accent-foreground", "border", "input", "tertiary",
  "chart-2", "chart-3", "chart-4", "series-other",
  "sidebar", "sidebar-foreground", "sidebar-primary", "sidebar-primary-foreground", "sidebar-accent", "sidebar-accent-foreground", "sidebar-border",
  "canvas-graph", "code", "raised",
] as const
export const STATUS_TOKENS = ["destructive", "attention", "success", "danger-icon", "chart-5", "series-1", "series-2", "series-3", "series-4", "series-5"] as const
export const SHADOW_TOKENS = ["lift", "lift-hover", "highlight", "sh-xs", "sh-sm", "sh-md", "sh-lg", "sh-xl"] as const
const COLOR = new Set<string>([...ACCENT_TOKENS, ...NEUTRAL_TOKENS, ...STATUS_TOKENS])
const SHADOW = new Set<string>(SHADOW_TOKENS)
/** the tokens each kind may set; fonts and radius have their own fields */
export const ALLOWED: Record<Kind, ReadonlySet<string>> = {
  accent: new Set(ACCENT_TOKENS),
  neutral: new Set([...NEUTRAL_TOKENS, ...SHADOW_TOKENS]),
  full: new Set([...COLOR, ...SHADOW]),
  sans: new Set(), mono: new Set(), radius: new Set(),
}

/* ───────── value syntax ───────── */
const HEX = /^#(?:[0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/i
const FN = /^(rgba?|hsla?|oklch|oklab|lab|lch)\(([^()]{1,80})\)$/i
const VAR = /^var\(--([a-z0-9-]+)\)$/
/** a colour literal, or a reference to another colour token (never to itself) */
export function isColor(v: string, self?: string): boolean {
  const s = v.trim()
  if (s === "transparent" || HEX.test(s)) return true
  const f = FN.exec(s)
  if (f) return /^[-+0-9.%\s,/]*$/.test(f[2].replace(/deg|grad|rad|turn|none/gi, ""))
  const r = VAR.exec(s)
  return !!r && COLOR.has(r[1]) && r[1] !== self
}
/** split on the commas that are not inside parentheses */
function layers(v: string) {
  const out: string[] = []; let depth = 0, cur = ""
  for (const ch of v) {
    if (ch === "(") depth++
    else if (ch === ")") depth--
    if (depth < 0) return null
    if (ch === "," && depth === 0) { out.push(cur.trim()); cur = "" } else cur += ch
  }
  return depth === 0 ? [...out, cur.trim()] : null
}
const LEN = /^-?(?:\d+|\d*\.\d+)(?:px)?$/
/** a box-shadow list: each layer `[inset] x y [blur [spread]] colour`, or `none` */
export function isShadow(v: string): boolean {
  const s = v.trim()
  if (s === "none") return true
  const ls = layers(s)
  if (!ls || ls.length > 6) return false
  return ls.every((l) => {
    const m = /^(.*?)\s*((?:rgba?|hsla?|oklch|oklab|lab|lch)\([^()]*\)|#[0-9a-f]+|transparent|var\(--[a-z0-9-]+\))$/i.exec(l)
    if (!m || !isColor(m[2])) return false
    const parts = m[1].split(/\s+/).filter(Boolean)
    if (parts[0] === "inset") parts.shift()
    return parts.length >= 2 && parts.length <= 4 && parts.every((p) => LEN.test(p))
  })
}
/** a font-family list of names: letters, digits, spaces, quotes, commas, dots, hyphens (CJK names too) */
export function isFontList(v: string): boolean {
  return v.length > 0 && v.length <= 300 && /^[\p{L}\p{N}\s"',._-]+$/u.test(v) && !/url|expression/i.test(v)
}

/* ───────── checking a manifest ───────── */
export type Checked = { ok: true; manifest: Manifest } | { ok: false; errors: string[] }
const ID = /^[a-z0-9][a-z0-9._-]{0,63}$/
const tokenName = (k: string) => k.replace(/^--/, "")
/** checks a manifest from anywhere (a built-in, a plugin, pasted JSON) and returns a clean copy, or why not */
export function validateManifest(input: unknown): Checked {
  const errors: string[] = []
  const m = input as Record<string, unknown> | null
  if (!m || typeof m !== "object" || Array.isArray(m)) return { ok: false, errors: ["not an object"] }
  const id = m.id, kind = m.kind as Kind
  if (typeof id !== "string" || !ID.test(id)) errors.push("id: lowercase letters, digits, . _ - (at most 64)")
  if (!KINDS.includes(kind)) errors.push(`kind: one of ${KINDS.join(", ")}`)
  const name: Record<string, string> = {}
  if (typeof m.name === "string" && m.name.trim()) name.en = m.name.trim().slice(0, 40)
  else if (m.name && typeof m.name === "object") for (const [l, v] of Object.entries(m.name)) if (typeof v === "string" && v.trim() && /^[a-zA-Z-]{2,10}$/.test(l)) name[l] = v.trim().slice(0, 40)
  if (!Object.keys(name).length) errors.push("name: a string, or { \"zh-CN\": …, \"en\": … }")
  const out: Manifest = { id: id as string, name, kind }
  for (const key of Object.keys(m)) if (!["id", "name", "kind", "tokens", "fonts", "radius", "version", "description"].includes(key)) errors.push(`${key}: not a manifest field`)
  if (m.tokens !== undefined) {
    const t = m.tokens as Record<string, unknown>
    if (!t || typeof t !== "object" || Array.isArray(t)) errors.push("tokens: { light: {…}, dark: {…} }")
    else {
      out.tokens = {}
      for (const [mode, set] of Object.entries(t)) {
        if (mode !== "light" && mode !== "dark") { errors.push(`tokens.${mode}: only light and dark`); continue }
        if (!set || typeof set !== "object" || Array.isArray(set)) { errors.push(`tokens.${mode}: an object`); continue }
        const clean: Tokens = {}
        const entries = Object.entries(set as Record<string, unknown>)
        if (entries.length > 80) errors.push(`tokens.${mode}: too many tokens`)
        for (const [k0, v] of entries) {
          const k = tokenName(k0)
          if (!ALLOWED[kind]?.has(k)) { errors.push(`tokens.${mode}.${k}: not a token a ${kind} theme may set`); continue }
          if (typeof v !== "string" || v.length > 400) { errors.push(`tokens.${mode}.${k}: a string`); continue }
          if (SHADOW.has(k) ? !isShadow(v) : !isColor(v, k)) { errors.push(`tokens.${mode}.${k}: "${v}" is not a ${SHADOW.has(k) ? "box-shadow" : "colour"}`); continue }
          clean[k] = v.trim()
        }
        out.tokens[mode] = clean
      }
    }
  }
  if (m.fonts !== undefined) {
    const f = m.fonts as Record<string, unknown>
    if (!f || typeof f !== "object" || Array.isArray(f)) errors.push("fonts: { sans?, mono? }")
    else {
      out.fonts = {}
      for (const [k, v] of Object.entries(f)) {
        const may = kind === "full" || kind === k
        if (k !== "sans" && k !== "mono") errors.push(`fonts.${k}: only sans and mono`)
        else if (!may) errors.push(`fonts.${k}: not something a ${kind} theme may set`)
        else if (typeof v !== "string" || !isFontList(v)) errors.push(`fonts.${k}: a font-family list of names`)
        else out.fonts[k] = v.trim()
      }
    }
  }
  if (m.radius !== undefined) {
    if (kind !== "full" && kind !== "radius") errors.push(`radius: not something a ${kind} theme may set`)
    else if (typeof m.radius !== "number" || !Number.isFinite(m.radius) || m.radius < 0 || m.radius > 2) errors.push("radius: a number from 0 to 2 (1 = standard)")
    else out.radius = m.radius
  }
  if (kind === "sans" && !out.fonts?.sans) errors.push("fonts.sans: a sans theme sets it")
  if (kind === "mono" && !out.fonts?.mono) errors.push("fonts.mono: a mono theme sets it")
  if (kind === "radius" && out.radius === undefined) errors.push("radius: a radius theme sets it")
  return errors.length ? { ok: false, errors } : { ok: true, manifest: out }
}

/* ───────── combining the chosen manifests ───────── */
/** Steve's corner radii at scale 1 (index.css); a radius theme scales these. The pill (--radius-4xl) never changes. */
export const RADII: Record<string, number> = { "radius-sm": 6, "radius-md": 6, "radius-lg": 8, "radius-xl": 12, "radius-2xl": 16, "radius-3xl": 18, radius: 12 }
export function radiusVars(scale: number): Record<string, string> {
  return Object.fromEntries(Object.entries(RADII).map(([k, px]) => [k, `${Math.round(px * scale)}px`]))
}
export type Resolved = { common: Record<string, string>; light: Record<string, string>; dark: Record<string, string> }
/** lays the manifests over each other in order (later wins): variables for both modes, and per mode */
export function combine(ms: readonly Manifest[]): Resolved {
  const r: Resolved = { common: {}, light: {}, dark: {} }
  for (const m of ms) {
    for (const mode of ["light", "dark"] as const) Object.assign(r[mode], m.tokens?.[mode] ?? {})
    if (m.fonts?.sans) { r.common["font-sans"] = m.fonts.sans; r.common["font-heading"] = m.fonts.sans }
    if (m.fonts?.mono) r.common["font-mono"] = m.fonts.mono
    if (m.radius !== undefined) Object.assign(r.common, radiusVars(m.radius))
  }
  return r
}
const decls = (o: Record<string, string>) => Object.entries(o).map(([k, v]) => `--${k}: ${v};`).join(" ")
/** the stylesheet that puts the variables on <html>: above :root and .dark in index.css, each mode its own */
export function toCss(r: Resolved): string {
  return [
    Object.keys(r.common).length ? `:root:root { ${decls(r.common)} }` : "",
    Object.keys(r.light).length ? `:root:root:not(.dark) { ${decls(r.light)} }` : "",
    Object.keys(r.dark).length ? `:root:root.dark { ${decls(r.dark)} }` : "",
  ].filter(Boolean).join("\n")
}
/** a token's final value in one mode, following var(--…) references through `base` (the stylesheet's values) */
export function tokenValue(name: string, mode: Mode, r: Resolved, base: Record<Mode, Record<string, string>>, depth = 0): string | undefined {
  const v = r[mode][name] ?? base[mode][name]
  const ref = v && VAR.exec(v)
  return ref && depth < 8 ? tokenValue(ref[1], mode, r, base, depth + 1) : v
}

/* ───────── contrast (WCAG 2) ───────── */
function lum(hex: string) {
  let h = hex.replace("#", "")
  if (h.length <= 4) h = [...h].map((c) => c + c).join("")
  const c = [0, 2, 4].map((n) => parseInt(h.slice(n, n + 2), 16) / 255).map((v) => (v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4))
  return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}
export const contrast = (a: string, b: string) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05) }
