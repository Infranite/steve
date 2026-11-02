/* Hourly chores: archive finished items after the workspace's archive period; give back worktrees of finished items. */
import { existsSync } from "node:fs"
import { db } from "./db.js"
import { items, settings, timeline } from "./repo.js"
import { tidyBranch } from "./orchestrator.js"
import { notify } from "./notify.js"

export function housekeeping() {
  for (const { id: ws } of db.prepare("SELECT id FROM workspaces").all() as { id: string }[]) {
    const days = Number(settings.get(ws, "archDays", "30"))
    const before = Date.now() - days * 24 * 3600_000
    const archived: string[] = []
    for (const i of items.all(ws)) {
      if (!["done", "drop"].includes(i.status)) continue
      if (days && !i.archived && i.updated < before) { items.patch(i.id, { archived: true }); timeline.add(i.id, "steve", "sys", `完成满 ${days} 天，自动归档`); archived.push(i.id) }
      if (i.cwd && existsSync(i.cwd)) try { tidyBranch(i.id) } catch (e) { console.error("tidy", i.id, (e as Error).message) }
    }
    /* one notification per run, not one per item */
    if (archived.length) notify(ws, "archive", "info", `自动归档了 ${archived.length} 件完成满 ${days} 天的事项：${archived.slice(0, 3).join("、")}${archived.length > 3 ? " 等" : ""}`, archived.length === 1 ? archived[0] : null)
  }
}
