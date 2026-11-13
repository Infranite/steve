/* Appearance: which theme manifests are chosen on this device, and putting them on <html>.
 *
 * The choice (accent, neutral, sans and mono font, radius, and an optional skin) is kept per device in
 * localStorage["steve-appearance"], like the light/dark choice in "steve-theme". The chosen manifests become one
 * <style id="steve-appearance"> whose rules sit on <html> above index.css's :root and .dark, light and dark each
 * their own, so the existing .dark switch keeps working and nothing reloads. A default choice adds nothing: the
 * stylesheet already is the default.
 *
 * registerTheme(manifest) is the hook for themes from elsewhere (a future "theme" plugin kind): it checks the
 * manifest (validateManifest) and refuses a bad one without touching what is on screen. A registered theme shows up
 * among the choices of its kind; a stored choice whose theme is not registered (yet) shows the default until it is. */
import { useSyncExternalStore } from "react"
import { BUILTIN } from "./builtin.ts"
import { combine, toCss, validateManifest, type Checked, type Kind, type Manifest } from "./manifest.ts"

export type { Manifest, Kind } from "./manifest.ts"
export { validateManifest } from "./manifest.ts"

export type Slot = "accent" | "neutral" | "sans" | "mono" | "radius" | "skin"
export type Appearance = Record<Slot, string>
export const DEFAULT_APPEARANCE: Appearance = { accent: "indigo", neutral: "slate", sans: "geist", mono: "geist-mono", radius: "standard", skin: "" }
/** the kind of manifest each choice takes */
export const SLOT_KIND: Record<Slot, Kind> = { accent: "accent", neutral: "neutral", sans: "sans", mono: "mono", radius: "radius", skin: "full" }
/** the order the chosen manifests are laid over each other: a skin goes over the colours, fonts and radius last */
const ORDER: Slot[] = ["neutral", "accent", "skin", "sans", "mono", "radius"]
const KEY = "steve-appearance"

type Entry = { manifest: Manifest; builtin: boolean }
const registry = new Map<string, Entry>()
for (const m of BUILTIN) registry.set(m.id, { manifest: m, builtin: true })

const hasDom = typeof document !== "undefined"
const storage = (): Storage | undefined => { try { return hasDom ? localStorage : undefined } catch { return undefined } }
function read(): Appearance {
  const out = { ...DEFAULT_APPEARANCE }
  try {
    const v = JSON.parse(storage()?.getItem(KEY) ?? "{}")
    if (v && typeof v === "object") for (const k of Object.keys(out) as Slot[]) if (typeof v[k] === "string") out[k] = v[k]
  } catch { /* a damaged value is the default */ }
  return out
}
let current = read()
let version = 0
const subs = new Set<() => void>()
const changed = () => { version++; snap = { ...current, version }; for (const f of subs) f() }
let snap: Appearance & { version: number } = { ...current, version }

/** the manifests that can be chosen for a slot, built-ins first in their order, then registered ones */
export function themesFor(slot: Slot): Manifest[] {
  const k = SLOT_KIND[slot]
  return [...registry.values()].filter((e) => e.manifest.kind === k).sort((a, b) => Number(b.builtin) - Number(a.builtin)).map((e) => e.manifest)
}
export const getTheme = (id: string): Manifest | undefined => registry.get(id)?.manifest
export const isBuiltin = (id: string) => !!registry.get(id)?.builtin
/** the manifest a slot uses now: the chosen one if it exists and fits the slot, otherwise the default */
export function chosen(slot: Slot, a: Appearance = current): Manifest | undefined {
  const m = registry.get(a[slot])?.manifest
  return m && m.kind === SLOT_KIND[slot] ? m : slot === "skin" ? undefined : registry.get(DEFAULT_APPEARANCE[slot])?.manifest
}
/** the manifests laid over the stylesheet for a choice, defaults left out */
export function layers(a: Appearance = current): Manifest[] {
  return ORDER.flatMap((s) => { const m = chosen(s, a); return m && m.id !== DEFAULT_APPEARANCE[s] ? [m] : [] })
}
/** the CSS the choice puts on <html> ("" for the defaults) */
export const appearanceCss = (a: Appearance = current) => toCss(combine(layers(a)))

export function applyAppearance() {
  if (!hasDom) return
  const css = appearanceCss()
  let el = document.getElementById("steve-appearance") as HTMLStyleElement | null
  if (!el) { el = document.createElement("style"); el.id = "steve-appearance"; document.head.appendChild(el) }
  if (el.textContent !== css) el.textContent = css
  const root = document.documentElement
  for (const s of ORDER) root.dataset[`theme${s[0].toUpperCase()}${s.slice(1)}`] = chosen(s)?.id ?? ""
}

export const getAppearance = (): Appearance => ({ ...current })
/** changes some of the choice on this device and shows it at once */
export function setAppearance(p: Partial<Appearance>) {
  current = { ...current, ...p }
  const diff = Object.fromEntries(Object.entries(current).filter(([k, v]) => DEFAULT_APPEARANCE[k as Slot] !== v))
  const ls = storage()
  if (Object.keys(diff).length) ls?.setItem(KEY, JSON.stringify(diff)); else ls?.removeItem(KEY)
  applyAppearance(); changed()
}
export const resetAppearance = () => setAppearance({ ...DEFAULT_APPEARANCE })

/** adds a theme from outside the built-ins (a plugin). A bad manifest, or one that would replace a built-in, is
 *  refused and nothing on screen changes; the same id again replaces the earlier registration. */
export function registerTheme(input: unknown): Checked {
  const c = validateManifest(input)
  if (!c.ok) { console.warn("[steve] theme refused:", c.errors.join("; ")); return c }
  if (registry.get(c.manifest.id)?.builtin) { const errors = [`id: "${c.manifest.id}" is a built-in theme`]; console.warn("[steve] theme refused:", errors[0]); return { ok: false, errors } }
  registry.set(c.manifest.id, { manifest: c.manifest, builtin: false })
  applyAppearance(); changed()
  return c
}
/** removes a registered theme; a choice that used it falls back to the default (and comes back if it returns) */
export function unregisterTheme(id: string) {
  const e = registry.get(id)
  if (!e || e.builtin) return false
  registry.delete(id); applyAppearance(); changed()
  return true
}

/** a theme's name in the language of the moment */
export function themeName(m: Manifest, lang: string) {
  return m.name[lang] ?? m.name[lang.split("-")[0]] ?? m.name.en ?? Object.values(m.name)[0] ?? m.id
}

const subscribe = (f: () => void) => { subs.add(f); return () => { subs.delete(f) } }
/** the current choice; re-renders when it, or the registered themes, change */
export const useAppearance = () => useSyncExternalStore(subscribe, () => snap, () => snap)

declare global { interface Window { steveThemes?: { registerTheme: typeof registerTheme; unregisterTheme: typeof unregisterTheme; validateManifest: typeof validateManifest } } }
if (hasDom) {
  /* the same hook for code that is not bundled with the client (a plugin's script) */
  window.steveThemes = { registerTheme, unregisterTheme, validateManifest }
  applyAppearance()
  /* another tab changed it */
  window.addEventListener("storage", (e) => { if (e.key === KEY) { current = read(); applyAppearance(); changed() } })
}
