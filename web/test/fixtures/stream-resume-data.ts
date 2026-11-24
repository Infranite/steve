/* Typed wire fixtures for stream-resume.browser.mjs. */
import type { Shell } from "../../src/api.ts"
import type { ById, Envelope, Patch, SessionView, Snapshot } from "../../src/gen/index.ts"
import { patchFrame, sessionView, snapshotViews, viewsResponse } from "./wire-views.ts"

export const streamResumeSession: SessionView = sessionView({
  id: "s1",
  ws: "ws",
  no: 1,
  harness: "codex",
  cwd: "/tmp",
  status: "running",
  used: 0,
  size: 0,
  title: "s1",
  device_id: "hub",
  source: "item",
  created: 1,
  updated: 2,
})

export const streamResumeShell: Shell = {
  modules: { feishuRegistration: false },
  version: "fixture",
  web_version: "fixture",
  workspace: { id: "ws", name: "ws", description: "", item_prefix: "ST", sort: 0, created: 0 },
  workspaces: [],
  bots: [],
  allBots: [],
  projects: [],
  tags: [],
  settings: {},
}

export function streamResumeSnapshot(epoch: string, head: number): Snapshot {
  return snapshotViews(
    { decision: [], item: [], session: [streamResumeSession] },
    { epoch, rev: 1, cursor: `${epoch}:${head}` },
  )
}

export function streamResumeEmptyViews(): ById {
  return viewsResponse([], 1)
}

export function streamResumeRemoveItemPatch(id: string, rev: number): Envelope<Patch> {
  return patchFrame([{ type: "item", op: "remove", id }], rev, "ws")
}
