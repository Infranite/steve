/* ── where a session sits in an item's activity feed (ST-42) ──
 * a session, and the agent's reply under a comment, are blocks that keep growing. Lines written while one runs
 * (plan changes, deliveries) go above it, so what is still moving stays at the bottom of its part of the feed.
 * A comment or the next session closes that part: a new request and the work after it stay apart from the work before. */
export type FeedRow = { ts: number; /** this row opens a block that runs until then (Infinity while it runs) */ until?: number; /** a comment: nothing moves past it */ fence?: boolean }

/** for each row that opens a block, the row the block is shown after (its own index when nothing moves); -1 for other rows */
export function blockSlots(rows: FeedRow[]): number[] {
  return rows.map((r, n) => {
    if (r.until == null) return -1
    let at = n
    while (at + 1 < rows.length) {
      const x = rows[at + 1]
      if (x.fence || x.until != null || x.ts > r.until) break
      at++
    }
    return at
  })
}
