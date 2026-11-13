/* What the item page says about its sessions' permission modes (GET /yolo/items/{id}): what Steve wants, what the
 * Node confirmed the harness runs in, and why the two differ. Nothing reads as in effect before the Node confirmed
 * it; full access waiting for the turn's end says that this turn's requests are answered by YOLO as full access. */
import type { ModeSource, ModeStatus, PermMode, RYoloItemSession } from "@/api/yolo"

export type ModeLine = {
  /** the i18n key under `yolo:mode.` */
  key: ModeStatus
  /** `ok`: in effect; `wait`: on its way or waiting; `bad`: refused or unknown */
  tone: "ok" | "wait" | "bad"
  wanted: PermMode | null
  /** where `wanted` comes from */
  from: ModeSource
  applied: PermMode | null
  reason: string
  residual: boolean
}

/** the line for one session; read-only sessions and ones nothing runs are left out */
export function modeLine(s: RYoloItemSession): ModeLine | null {
  if (s.readonly || s.status === "not_running") return null
  /* a mode Steve cannot name is never in effect, whatever an older Hub says */
  const key: ModeStatus = s.status === "applied" && !s.wanted ? "unknown" : s.status
  const tone = key === "applied" ? "ok" : key === "refused" || key === "unknown" ? "bad" : "wait"
  return { key, tone, wanted: s.wanted, from: s.wanted_from, applied: s.applied, reason: s.refused ?? "", residual: s.residual }
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

/** Only the newest request's answer counts: `begin` before sending one returns whether it is still the newest when
 *  its answer comes; `drop` voids every request sent so far (the item changed, the view went away). An answer that
 *  arrives late, after a newer request or for the item shown before, is never shown. */
export function newest() {
  let n = 0
  return {
    begin() { const k = ++n; return () => k === n },
    drop() { n++ },
  }
}
