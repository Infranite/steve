/* Plugins & plugin theme manifests (GET /plugins, /plugins/themes, store & install actions). */
import { modReq } from "@/api"

/** a library's or a plugin's words: one string, or one per language */
export type PluginText = string | Record<string, string>
export type PluginConfigField = { key: string; label: PluginText; hint?: PluginText; secret: boolean; required: boolean }
export type PluginState = "starting" | "running" | "waiting" | "stopped"
export type PluginInstance = { instance: string; connection: string | null; status: { state: PluginState; pid?: number; error?: string } }
export type Plugin = {
  id: string; name: PluginText; summary: PluginText; kind: string; version: string; homepage: string | null; config: PluginConfigField[]
  /** false: installed, and the library no longer lists it */
  listed: boolean
  /** this Hub cannot run it: its kind or its contract is from a newer Steve */
  unsupported: "kind" | "contract" | null
  /** the library has a build for the Hub's machine (or one for any machine) */
  available: boolean
  installed: { version: string; installed: number; config: PluginConfigField[] } | null; update: boolean
  /** the caller's own */
  connections: PluginInstance[]
}
/** `address` and `error` are the owner's to see; anyone else gets `failed` */
export type PluginStore = { id: string; name: PluginText | null; checked: number | null; official: boolean; plugins: Plugin[]; address?: string; error?: string | null; failed?: boolean }
export type PluginsView = { target: string; owner: boolean; stores: PluginStore[] }
export type PluginTheme = { store: string; plugin: string; version: string; manifest: unknown }

const e = encodeURIComponent
export const pluginsApi = {
  list: () => modReq<PluginsView>("GET", "/plugins"),
  themes: () => modReq<{ themes: PluginTheme[] }>("GET", "/plugins/themes"),
  addStore: (address: string) => modReq<{ id: string }>("POST", "/plugins/stores", { address }),
  /** `error`: the index could not be read this time; what was read before stays */
  refresh: (store: string) => modReq<{ id: string; checked: number | null; error: string | null }>("POST", `/plugins/stores/${e(store)}/refresh`, {}),
  removeStore: (store: string) => modReq<{ ok: true }>("DELETE", `/plugins/stores/${e(store)}`),
  /** also the update: installs the version the library names now */
  install: (store: string, plugin: string) => modReq<{ version: string; installed: number }>("POST", `/plugins/installed/${e(store)}/${e(plugin)}`, {}),
  uninstall: (store: string, plugin: string) => modReq<{ ok: true }>("DELETE", `/plugins/installed/${e(store)}/${e(plugin)}`),
  /** the settings go straight into the body, never anywhere else */
  connect: (store: string, plugin: string, config: Record<string, string>) => modReq<{ instance: string; connection: string }>("POST", `/plugins/installed/${e(store)}/${e(plugin)}/connect`, { config }),
}
