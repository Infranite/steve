import type { RFile } from "./api"

export type DraftMention = [string, { kind: "party" | "bot"; id: string }]
/** `clientId`: the send idempotency key of this draft, made at its first send and kept while the draft stays the
 * same, so a retry (failed answer, reload, second click) is the same send to the Hub; editing the draft drops it */
export type ComposerDraft = { text: string; files: RFile[]; mentions: DraftMention[]; clientId?: string }
const memory = new Map<string, ComposerDraft>()
const prefix = "steve:composer:v1:"
export const draftKey = (workspace: string | undefined, kind: "chat" | "session" | "item", id = "") =>
  JSON.stringify([workspace ?? "", kind, id])

/** Storage may be unavailable or full; keep in-memory navigation working in that case. */
export function readDraft(key?: string): ComposerDraft {
  const empty = () => ({ text: "", files: [], mentions: [] })
  if (!key) return empty()
  if (memory.has(key)) return memory.get(key)!
  try {
    const value = JSON.parse(localStorage.getItem(prefix + key) ?? "null")
    if (value && typeof value.text === "string" && Array.isArray(value.files) && Array.isArray(value.mentions)) {
      const draft: ComposerDraft = { text: value.text, files: value.files.filter((f: RFile) => f && typeof f.id === "string" && typeof f.name === "string" && typeof f.mime === "string" && typeof f.size === "number"), mentions: value.mentions.filter((m: DraftMention) => Array.isArray(m) && typeof m[0] === "string" && m[1] && ["party", "bot"].includes(m[1].kind) && typeof m[1].id === "string") }
      if (typeof value.clientId === "string" && value.clientId) draft.clientId = value.clientId
      memory.set(key, draft)
      return draft
    }
  } catch { /* malformed or inaccessible storage is an empty draft */ }
  return empty()
}

/** the same text and attachments: what the send idempotency key stands for */
const sameSend = (a: ComposerDraft, b: ComposerDraft) =>
  a.text === b.text && a.files.length === b.files.length && a.files.every((f, i) => f.id === b.files[i].id)

export const newClientId = (): string => crypto.randomUUID?.() ?? `${Date.now()}-${Math.random().toString(36).slice(2)}`

export function saveDraft(key: string | undefined, draft: ComposerDraft) {
  if (!key) return
  if (draft.clientId === undefined) {
    const before = readDraft(key)
    if (before.clientId && sameSend(before, draft)) draft = { ...draft, clientId: before.clientId }
  }
  memory.set(key, draft)
  try {
    if (!draft.text && !draft.files.length) localStorage.removeItem(prefix + key)
    else localStorage.setItem(prefix + key, JSON.stringify(draft))
  } catch { /* the in-memory draft remains available */ }
}

/** The draft about to be sent, with its idempotency key: the stored one while the draft is unchanged, else a new one. */
export function draftToSend(key: string | undefined, draft: ComposerDraft): ComposerDraft & { clientId: string } {
  const before = readDraft(key)
  const clientId = (before.clientId && sameSend(before, draft) ? before.clientId : undefined) ?? newClientId()
  const out = { ...draft, clientId }
  saveDraft(key, out)
  return out
}

const content = (d: ComposerDraft) => JSON.stringify([d.text, d.files, d.mentions])

/** An earlier send may finish after navigation; never erase a newer draft. */
export function clearSentDraft(key: string | undefined, sent: ComposerDraft) {
  if (!key) return true
  if (content(readDraft(key)) !== content(sent)) return false
  saveDraft(key, { text: "", files: [], mentions: [] })
  return true
}
