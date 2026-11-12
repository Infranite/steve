/* What the item page says about its sessions' permission modes (GET /yolo/items/{id}): what Steve wants, what the
 * Node confirmed the harness runs in, and why the two differ. Nothing reads as in effect before the Node confirmed
 * it; full access waiting for the turn's end says that this turn's requests are answered by YOLO as full access. */
import type { ModeStatus, PermMode, RYoloItemSession } from "@/api/yolo"

export type ModeLine = {
  /** the i18n key under `yolo:mode.` */
  key: ModeStatus
  /** `ok`: in effect; `wait`: on its way or waiting; `bad`: refused or unknown */
  tone: "ok" | "wait" | "bad"
  wanted: PermMode | null
  applied: PermMode | null
  reason: string
  residual: boolean
}

/** the line for one session; read-only sessions and ones nothing runs are left out */
export function modeLine(s: RYoloItemSession): ModeLine | null {
  if (s.readonly || s.status === "not_running") return null
  const tone = s.status === "applied" ? "ok" : s.status === "refused" || s.status === "unknown" ? "bad" : "wait"
  return { key: s.status, tone, wanted: s.wanted, applied: s.applied, reason: s.refused ?? "", residual: s.residual }
}

/** the lines of an item's sessions, one per distinct line (several sessions in the same state read once) */
export function modeLines(sessions: RYoloItemSession[]): ModeLine[] {
  const seen = new Set<string>()
  const out: ModeLine[] = []
  for (const s of sessions) {
    const l = modeLine(s)
    if (!l) continue
    const k = JSON.stringify(l)
    if (seen.has(k)) continue
    seen.add(k)
    out.push(l)
  }
  return out
}
