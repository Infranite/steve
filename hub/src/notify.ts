/* Notifications: things that happened (a release went out, items were archived, the Hub restarted) rather than
 * conversation. They live in the messages table under space "notify", so no migration is needed and old rows stay readable.
 * A failure also leaves one short line in the main conversation, pointing back at the notification.
 * Before this, release results were plain Steve messages in the conversation: those are recognised by their text and
 * listed here as well; the successful ones are left out of the conversation.
 * A failure stops asking for attention once it is closed: the user dismissed it, or a later release notice of the same
 * item (it went out, or failed again) took its place. Its line then leaves the conversation. */
import { db, now } from "./db.js"
import { messages, type Message } from "./repo.js"
import { publish } from "./bus.js"

export type NoticeKind = "release" | "archive" | "system"
export type NoticeLevel = "info" | "success" | "fail"
export type Notice = { id: string; kind: NoticeKind; level: NoticeLevel; text: string; item_id: string | null; created: number; closed: boolean }

const SPACE = "notify", SEEN = "notifySeen", DISMISSED = "noticeDismissed", WINDOW = 500
const LEGACY_OK = /^已发布新版本 /, LEGACY_FAIL = /^发布 .*失败，已回到 /
const legacy = (m: Message) => m.author === "steve" && !m.meta && (LEGACY_OK.test(m.text) || LEGACY_FAIL.test(m.text))

export function notify(ws: string, kind: NoticeKind, level: NoticeLevel, text: string, item: string | null = null) {
  const id = messages.add(ws, "steve", text, item, { notice: { kind, level } }, SPACE)
  if (level === "fail") messages.add(ws, "steve", text, item, { notice: { kind, level, ref: id } })
  return id
}

/** ids the user closed by hand, oldest first */
function dismissed(ws: string): string[] {
  const r = db.prepare("SELECT value FROM settings WHERE workspace_id = ? AND key = ?").get(ws, DISMISSED) as { value: string } | undefined
  try { const v = r ? JSON.parse(r.value) : []; return Array.isArray(v) ? v : [] } catch { return [] }
}
/** notify rows that no longer ask for attention: dismissed, or a failure followed by a later release notice of the same item */
function closed(ws: string, rows: Message[]): Set<string> {
  const out = new Set(dismissed(ws)), asc = [...rows].sort((a, b) => a.created - b.created)
  asc.forEach((m, n) => {
    if (m.item_id && m.meta?.notice?.level === "fail" && m.meta?.notice?.kind === "release" && asc.slice(n + 1).some((x) => x.item_id === m.item_id && x.meta?.notice?.kind === "release")) out.add(m.id)
  })
  return out
}
/** the user closes a notice (a notify row id, or its line in the conversation): it stays in the list, unpinned, and its line leaves the conversation */
export function dismiss(ws: string, id: string) {
  const ref = messages.get(id)?.meta?.notice?.ref ?? id, ids = dismissed(ws).filter((x) => x !== ref).concat(ref).slice(-WINDOW)
  db.prepare("INSERT INTO settings (workspace_id, key, value, updated) VALUES (?, ?, ?, ?) ON CONFLICT(workspace_id, key) DO UPDATE SET value = excluded.value, updated = excluded.updated").run(ws, DISMISSED, JSON.stringify(ids), now())
  publish({ type: "settings", id: ws })
}

export function list(ws: string, limit = 100): Notice[] {
  const all = messages.list(ws, SPACE, WINDOW), shut = closed(ws, all)
  const rows = [...all.slice(-limit), ...messages.list(ws, "steve", 500).filter(legacy)]
  return rows.reverse().sort((a, b) => b.created - a.created).slice(0, limit).map((m) => ({
    id: m.id, kind: m.meta?.notice?.kind ?? "release", level: m.meta?.notice?.level ?? (LEGACY_FAIL.test(m.text) ? "fail" : "success"), text: m.text, item_id: m.item_id, created: m.created, closed: shut.has(m.id),
  }))
}
/** the conversation without the notices that used to be written into it (successful releases) and without the lines of closed failures */
export function thread(ws: string) {
  const shut = closed(ws, messages.list(ws, SPACE, WINDOW))
  return messages.list(ws).filter((m) => !(legacy(m) && LEGACY_OK.test(m.text)) && !(m.meta?.notice?.ref && shut.has(m.meta.notice.ref)))
}

/** everything up to `at` has been seen; one cursor per workspace, stored with the settings but without a revision */
export function seen(ws: string) {
  const r = db.prepare("SELECT value FROM settings WHERE workspace_id = ? AND key = ?").get(ws, SEEN) as { value: string } | undefined
  if (r) return Number(r.value)
  /* first look after the upgrade: what was already in the conversation has been seen there */
  const at = now(); db.prepare("INSERT INTO settings (workspace_id, key, value, updated) VALUES (?, ?, ?, ?)").run(ws, SEEN, JSON.stringify(at), at); return at
}
export function markSeen(ws: string, at = now()) {
  db.prepare("INSERT INTO settings (workspace_id, key, value, updated) VALUES (?, ?, ?, ?) ON CONFLICT(workspace_id, key) DO UPDATE SET value = excluded.value, updated = excluded.updated").run(ws, SEEN, JSON.stringify(at), now())
  publish({ type: "settings", id: ws }); return at
}
