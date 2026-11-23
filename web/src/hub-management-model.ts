export type HubMetadata = { hub_id: string; display_name: string; purpose_note: string; revision: number; updated_at: number | null; updated_by: string | null }
/** one address of the Hub: its endpoint, where its pages are, and the route it is on */
export type HubAddress = { url: string; page: string; kind: "public" | "lan" | "tailnet" | "loopback" }
export type HubInfo = { metadata: HubMetadata; can_edit: boolean; edit_reason: string | null; device_id: string | null; addresses: HubAddress[] }
export type HubDraft = Pick<HubMetadata, "display_name" | "purpose_note">
/** The inventory explicitly declares the local binding; a Hub role or name is not an identity join.
 * Ambiguous bindings stay unknown rather than linking to an arbitrary device. */
export function linkedHubDevice(devices: readonly { id: string; name: string; node_id?: string | null }[]) {
  const local = devices.filter(device => device.node_id === "local")
  return local.length === 1 ? local[0] : undefined
}
/** Where harnesses are set up: the Hub's own Node when the binding is known, else the Node list. */
export const runtimeSetupHrefFor = (devices: Parameters<typeof linkedHubDevice>[0]) => (linkedHubDevice(devices) ? "#/devices/nodes/local" : "#/devices/nodes")
export function normalizeHubDraft(d: HubDraft): HubDraft {
  return { display_name: d.display_name.trim(), purpose_note: d.purpose_note.replace(/\r\n?/g, "\n") }
}
export function hubDraftErrors(d: HubDraft): { name: boolean; note: boolean } {
  const n = normalizeHubDraft(d)
  return { name: [...n.display_name].length < 1 || [...n.display_name].length > 64 || /[\p{Cc}\u2028\u2029]/u.test(n.display_name), note: [...n.purpose_note].length > 500 || /[\p{Cc}]/u.test(n.purpose_note.replace(/[\n\t]/g, "")) }
}
/** Never turn a success that is not Hub metadata into a successful save. */
export function parseHubInfo(raw: unknown): HubInfo {
  if (!raw || typeof raw !== "object" || !("metadata" in raw) || !raw.metadata || typeof raw.metadata !== "object") throw new Error("Invalid Hub metadata")
  const m = raw.metadata
  if (!("hub_id" in m) || typeof m.hub_id !== "string" || !m.hub_id || !("display_name" in m) || typeof m.display_name !== "string" || !("purpose_note" in m) || typeof m.purpose_note !== "string" || !("revision" in m) || typeof m.revision !== "number" || !Number.isSafeInteger(m.revision) || m.revision < 0 || !("updated_at" in m) || (m.updated_at !== null && typeof m.updated_at !== "number") || !("updated_by" in m) || (m.updated_by !== null && typeof m.updated_by !== "string") || !("can_edit" in raw) || typeof raw.can_edit !== "boolean") throw new Error("Invalid Hub metadata")
  return raw as HubInfo
}

export type HubSnapshot = { metadata: HubMetadata; seen: number }
/** A snapshot carries no editable permission; malformed/old local data is never rendered as a Hub. */
export function parseHubSnapshot(raw: unknown): HubSnapshot | null {
  if (!raw || typeof raw !== "object" || !("seen" in raw) || typeof raw.seen !== "number" || !Number.isFinite(raw.seen) || !("metadata" in raw)) return null
  try {
    const info = parseHubInfo({ metadata: raw.metadata, can_edit: false })
    return { metadata: info.metadata, seen: raw.seen }
  } catch { return null }
}
