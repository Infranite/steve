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
/** Steve's permission modes, as a session's harness runs in them */
export type PermMode = "plan" | "ask" | "accept_edits" | "auto" | "full_access"
/** where one session's harness stands: `applied` only once its backend (the Node) confirmed it */
export type ModeStatus = "applied" | "after_turn" | "sending" | "next_open" | "refused" | "unknown" | "not_sent" | "not_running"
/** where the mode Steve wants comes from: full access, a bot set to bypass prompts, the bot's own mode, no bot (the
 *  asking mode a Node opens a session in without one), a read-only session */
export type ModeSource = "full_access" | "bot_bypass" | "bot" | "node_default" | "readonly"
export type RYoloItemSession = {
  session_id: string; readonly: boolean; status: ModeStatus
  /** what Steve asks for now (null: this runtime cannot name it; never in effect then) */
  wanted: PermMode | null
  wanted_from: ModeSource
  /** what the Node confirmed the harness runs in (null: nothing confirmed yet) */
  applied: PermMode | null
  /** waiting for the turn's end or the next open, or on its way */
  desired: PermMode | null
  busy: boolean
  /** what the Node or the harness said of a change that did not take */
  refused: string | null
  /** full access is wanted but this turn still runs in a mode that asks: YOLO answers what it asks as full access */
  residual: boolean
}
/** GET /yolo/items/{id}: the level the item resolves to and the layer that decided, and its sessions' modes */
export type RYoloItem = {
  item_id: string; level: YoloLevel
  source: "forced" | "paused" | "item" | "project" | "project_default" | "workspace"
  layers: { workspace: YoloMode; project: { mode: YoloMode | null; real_dir: boolean } | null; item: YoloMode | null; forced: string | null; paused: boolean }
  sessions: RYoloItemSession[]
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
  /** `reason` is display text in the UI language; these are what the web branches on:
   *  the user clicked 我来定, and the hard limit's own text when the row was stopped by one */
  user_took_over?: boolean; limit_what?: string | null
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
  item: (id: string) => modReq<RYoloItem>("GET", `/yolo/items/${e(id)}`),
  resume: () => modReq<RYolo>("POST", "/yolo/resume"),
  actions: (p: { window?: string; item?: string; outcome?: string } = {}) => modReq<RYoloDigest>("GET", "/yolo/actions" + q(p)),
  undo: (id: string, text?: string) => modReq<RYoloAction>("POST", `/yolo/actions/${e(id)}/undo`, text === undefined ? {} : { text }),
  now: (id: string) => modReq<RYoloAction>("POST", `/yolo/actions/${e(id)}/now`),
  cancel: (id: string) => modReq<RYoloAction>("POST", `/yolo/actions/${e(id)}/cancel`),
}
