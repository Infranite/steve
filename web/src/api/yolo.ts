/* YOLO (modules/yolo.md §7, with the user's decisions of 2026-11-01: on/off without an end time, no budgets): the
 * workspace switch and its preview, project and item scopes, resuming after the breaker, and the ledger with undo
 * and the cool-down buttons. The TS Hub has none of it: `NotWired` makes the YOLO UI stay hidden. */
import { modReq, qs as q } from "@/api"

const e = encodeURIComponent

export type YoloMode = "on" | "off" | "inherit" | "full"
/** the workspace switch: off, YOLO (Steve decides what can be undone), full access (agents ask nothing) */
export type YoloLevel = "off" | "on" | "full"
export type RYoloScope = { object_type: "project" | "item"; object_id: string; mode: YoloMode; reason: string; updated?: number; default?: boolean }
export type RYolo = {
  on: boolean
  /** the workspace switch as set (full access counts as on) */
  mode?: YoloLevel
  full?: boolean
  /** full access applies now (not forced off, not paused) */
  full_active?: boolean
  /** on, not forced off, not paused: YOLO is deciding */
  active: boolean
  forced_off: "env" | "no_resume" | null
  forced_reason: string | null
  paused: { code: string; reason: string; at: number } | null
  window: { id: string; started: number; include_open: boolean } | null
  scopes: RYoloScope[]
  counts: { decided: number; declined: number; scheduled: number }
}
export type RYoloPreview = {
  preview: true
  would_handle: { decision_id: string; question: string; choice: string; rule: string; delay_s: number }[]
  keep_for_you: number; hard_limited: number; muted: number
}
export type UndoKind = "decision" | "rollback" | "recall" | "reanswer" | "pause"
export type RYoloAction = {
  id: string; workspace_id: string; window_id: string | null; decision_id: string | null; item_id: string | null; session_id: string | null
  kind: string; subject_type: string; rule: string
  outcome: "scheduled" | "decided" | "declined" | "muted" | "cancelled" | "failed"
  choice: string | null; text: string | null; reason: string; checks: any
  due_at: number | null; done_at: number | null; undo: { kind: UndoKind } | null; undone: number | null; created: number
  /** the card's question, its status now, and the undo it offers now (null: none) */
  question: string | null; decision_status: string | null; can_undo: UndoKind | null
}
export type RYoloWindow = {
  id: string; started: number; ended: number | null; ended_why: "off" | "tripped" | null
  counts: Partial<Record<RYoloAction["outcome"], number>>; summary: string; releases: number; cost: number | null
  /** cards waiting for you (the running window only) */
  waiting: number | null
  items: { item_id: string | null; title: string | null; status: string | null; actions: RYoloAction[] }[]
}
export type RYoloDigest = { windows: RYoloWindow[]; waiting: number }

export const yoloApi = {
  status: () => modReq<RYolo>("GET", "/yolo"),
  preview: () => modReq<RYoloPreview>("PUT", "/yolo", { on: true, preview: true }),
  /** full access needs `confirm`: the Hub refuses it otherwise */
  set: (mode: YoloLevel, include_open = false) => modReq<RYolo>("PUT", "/yolo", mode === "off" ? { mode } : { mode, include_open, ...(mode === "full" ? { confirm: true } : {}) }),
  scope: (type: "project" | "item", id: string, mode: YoloMode) => modReq<RYolo>("PUT", `/yolo/scopes/${type}/${e(id)}`, mode === "full" ? { mode, confirm: true } : { mode }),
  resume: () => modReq<RYolo>("POST", "/yolo/resume"),
  actions: (p: { window?: string; item?: string; outcome?: string } = {}) => modReq<RYoloDigest>("GET", "/yolo/actions" + q(p)),
  undo: (id: string, text?: string) => modReq<RYoloAction>("POST", `/yolo/actions/${e(id)}/undo`, text === undefined ? {} : { text }),
  now: (id: string) => modReq<RYoloAction>("POST", `/yolo/actions/${e(id)}/now`),
  cancel: (id: string) => modReq<RYoloAction>("POST", `/yolo/actions/${e(id)}/cancel`),
}
