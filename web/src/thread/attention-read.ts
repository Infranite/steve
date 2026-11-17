import type { AttentionCategory, AttentionClient, AttentionEntry } from "@/api/attention"

/** A read cursor advances only to a final reply both present in this snapshot and actually on screen. */
export function visibleReply(entries: AttentionEntry[], visible: ReadonlySet<string>): AttentionEntry | undefined {
  return entries.filter((e) => visible.has(e.target.entity_id) && Number.isSafeInteger(e.order))
    .reduce<AttentionEntry | undefined>((last, e) => !last || e.order! > last.order! ? e : last, undefined)
}

/** Do not combine pages from different projections, including when the caller/workspace was replaced. */
export async function replySnapshot(client: AttentionClient, workspace: string, signal: AbortSignal) {
  let category: AttentionCategory | undefined, identity = "", cursor: string | undefined
  const cursors = new Set<string>(), ids = new Set<string>(), entries: AttentionEntry[] = []
  do {
    signal.throwIfAborted()
    const page = await client.replies(cursor, signal)
    const next = page.categories.find((c) => c.kind === "replies")
    if (page.v !== 1 || page.workspace_id !== workspace || !page.user_id || !page.epoch || !next?.available || next.unchanged
      || next.unit !== "messages" || !Number.isSafeInteger(next.count) || next.count < 0) throw new Error("Invalid reply snapshot")
    const key = JSON.stringify([page.epoch, page.user_id, page.workspace_id, next.revision, next.count])
    if (identity && identity !== key) throw new Error("Reply snapshot changed during pagination")
    identity = key; category = next
    for (const entry of next.entries) if (!ids.has(entry.id)) { ids.add(entry.id); entries.push(entry) }
    cursor = next.next_cursor ?? undefined
    if (cursor) { if (cursors.has(cursor)) throw new Error("Repeated reply cursor"); cursors.add(cursor) }
  } while (cursor)
  signal.throwIfAborted()
  if (!category?.snapshot) throw new Error("Incomplete reply snapshot")
  return { snapshot: category.snapshot, entries }
}

/** Partly visible long replies count, but offscreen rows and background windows do not. */
export function visibleReplyIDs(root: HTMLElement): Set<string> {
  const viewport = root.getBoundingClientRect(), ids = new Set<string>()
  if (!viewport.width || !viewport.height) return ids
  for (const el of root.querySelectorAll<HTMLElement>("[data-msg]")) {
    const r = el.getBoundingClientRect()
    if (r.bottom > Math.max(0, viewport.top) && r.top < Math.min(window.innerHeight, viewport.bottom)
      && r.right > Math.max(0, viewport.left) && r.left < Math.min(window.innerWidth, viewport.right)
      && el.dataset.msg) ids.add(el.dataset.msg)
  }
  return ids
}
