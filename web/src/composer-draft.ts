import type { RFile } from "./api"

export type DraftMention = [string, { kind: "party" | "bot"; id: string }]
export type ComposerDraft = { text: string; files: RFile[]; mentions: DraftMention[] }
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
      const draft = { text: value.text, files: value.files.filter((f: RFile) => f && typeof f.id === "string" && typeof f.name === "string" && typeof f.mime === "string" && typeof f.size === "number"), mentions: value.mentions.filter((m: DraftMention) => Array.isArray(m) && typeof m[0] === "string" && m[1] && ["party", "bot"].includes(m[1].kind) && typeof m[1].id === "string") }
      memory.set(key, draft)
      return draft
    }
  } catch { /* malformed or inaccessible storage is an empty draft */ }
  return empty()
}

export function saveDraft(key: string | undefined, draft: ComposerDraft) {
  if (!key) return
  memory.set(key, draft)
  try {
    if (!draft.text && !draft.files.length) localStorage.removeItem(prefix + key)
    else localStorage.setItem(prefix + key, JSON.stringify(draft))
  } catch { /* the in-memory draft remains available */ }
}

/** An earlier send may finish after navigation; never erase a newer draft. */
export function clearSentDraft(key: string | undefined, sent: ComposerDraft) {
  if (!key) return true
  if (JSON.stringify(readDraft(key)) !== JSON.stringify(sent)) return false
  saveDraft(key, { text: "", files: [], mentions: [] })
  return true
}
