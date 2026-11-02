/* In-process domain events. The SSE stream and the orchestrator subscribe here. */
import { EventEmitter } from "node:events"

export type HubEvent =
  | { type: "item"; id: string }
  | { type: "decision"; id: string; item_id?: string | null }
  | { type: "session"; id: string; item_id?: string | null }
  | { type: "event"; session_id: string; seq: number; kind: string; data: unknown }
  | { type: "message"; id: string }
  | { type: "steve.thinking"; on: boolean; ws?: string; phase?: "route" | "answer" | "config" | "item" | "supplement" }
  | { type: "bot" | "project" | "tag" | "workspace" | "settings" | "device"; id: string }

const ee = new EventEmitter()
ee.setMaxListeners(200)
export const publish = (e: HubEvent) => ee.emit("e", e)
export const subscribe = (f: (e: HubEvent) => void) => { ee.on("e", f); return () => ee.off("e", f) }
