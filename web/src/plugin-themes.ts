/* Theme plugins: skins installed on the Hub (a plugin of kind "theme", hub-rs steve-hub plugins/mod.rs) become choices in
 * 设置 · 外观 on every device. The Hub only hands over each installed theme's manifest (GET /plugins/themes); here each
 * one is checked and registered with registerTheme, exactly like a built-in: data only, nothing of the plugin runs in
 * the browser. A theme whose plugin was uninstalled is unregistered, and a choice that used it falls back to the default.
 * What each plugin's manifest came to (registered, or why not) is kept for the Plugins page to show. */
import { useSyncExternalStore } from "react"
import { modReq } from "./api"
import { registerTheme, unregisterTheme } from "./themes"

export type PluginTheme = { store: string; plugin: string; version: string; manifest: unknown }
/** a plugin's theme on this client: its id once registered, otherwise why it was refused */
export type ThemeState = { id?: string; errors?: string[] }

let registered = new Set<string>()
let states: Record<string, ThemeState> = {}
const subs = new Set<() => void>()
const key = (store: string, plugin: string) => `${store}/${plugin}`

/** reads the installed theme plugins again and brings the registered themes in line with them */
export async function syncPluginThemes(): Promise<void> {
  let themes: PluginTheme[]
  try { themes = (await modReq<{ themes: PluginTheme[] }>("GET", "/plugins/themes")).themes ?? [] } catch { return } // a Hub without plugins, or signed out: nothing to add
  const now = new Set<string>(), next: Record<string, ThemeState> = {}
  for (const t of themes) {
    const r = t.manifest == null ? { ok: false as const, errors: ["the manifest could not be read"] } : registerTheme(t.manifest)
    if (r.ok) { now.add(r.manifest.id); next[key(t.store, t.plugin)] = { id: r.manifest.id } }
    else next[key(t.store, t.plugin)] = { errors: r.errors }
  }
  for (const id of registered) if (!now.has(id)) unregisterTheme(id)
  registered = now; states = next
  for (const f of subs) f()
}

const subscribe = (f: () => void) => { subs.add(f); return () => { subs.delete(f) } }
/** what an installed theme plugin came to on this client (undefined until the themes were read) */
export const usePluginTheme = (store: string, plugin: string): ThemeState | undefined =>
  useSyncExternalStore(subscribe, () => states[key(store, plugin)], () => undefined)
