// Acceptance: drive the real Hub + real Claude Code through the item main line over HTTP, and check the workspace boundary.
// Usage: node test/e2e-mainline.mjs <base-url>   (a throwaway Hub started with STEVE_DEV_LOGIN=1; see test/e2e.sh)
import { readFileSync, existsSync, rmSync } from "node:fs"
import { join } from "node:path"
import { login, call, statusOf, log, waitFor, openOf, check, setup, handle } from "./lib.mjs"

await login()
const { dir, project, bot } = await setup("main")
const item = await call("POST", "/api/items", { goal: "在当前目录用 Write 工具（不要用 shell 命令）新建 hello.txt，内容是 hi（不要换行以外的其他内容）。完成后用 steve_deliver 交付。", title: "写 hello.txt", bot: bot.id, project: project.id })
log("item", item.id, item.status, "bot", item.bot_id, "project", item.project_id)
check(item.bot_id === bot.id && item.project_id === project.id, "item has the chosen bot and project")

// the workspace boundary: another workspace sees none of it
const other = await call("POST", "/api/workspaces", { name: "e2e-other-" + Date.now().toString(36) })
check(await statusOf("GET", "/api/items/" + item.id, null, other.id) === 404, "item is invisible from another workspace")
check(await statusOf("PATCH", "/api/bots/" + bot.id, { role: "x" }, other.id) === 404, "bot of another workspace can't be changed")
check(await statusOf("POST", "/api/items/" + item.id + "/actions", { action: "pause" }, other.id) === 404, "item of another workspace can't be paused")
const ost = await call("GET", "/api/state", null, other.id)
check(ost.workspace.id === other.id && !ost.items.some((i) => i.id === item.id) && !ost.projects.some((p) => p.id === project.id) && !ost.bots.some((b) => b.id === bot.id), "other workspace's /state is clean")

// delivery → acceptance decision; a permission request is inspected, never blindly approved
let acc
while (!acc) {
  const ds = await waitFor(async () => { const x = await openOf(item.id); return x.length ? x : null }, "a decision")
  for (const d of ds) acc = (await handle(d, dir)) ?? acc
}
log("deliver:", acc.draft.slice(0, 160))
const d1 = await call("GET", "/api/items/" + item.id)
check(d1.status === "wait" && d1.stage === "交付与验收", "item waits for acceptance")
check(d1.cwd === dir && d1.branch === null, "session ran in the project directory")
check(existsSync(join(dir, "hello.txt")) && readFileSync(join(dir, "hello.txt"), "utf8").trim() === "hi", "hello.txt written")
log("now:", d1.now, "| plan:", JSON.stringify(d1.plan), "| artifacts:", d1.artifacts.map((a) => a.title).join(", "))
const ses = await call("GET", "/api/sessions/" + d1.sessionIds[0])
check(ses.bot_id === bot.id && ses.item_id === item.id, "session belongs to the item and the bot")
const evs = await call("GET", `/api/sessions/${ses.id}/events`)
const kinds = {}; evs.forEach((e) => (kinds[e.kind] = (kinds[e.kind] ?? 0) + 1)); log("events:", JSON.stringify(kinds), "| model:", ses.model, "| usage:", ses.used, "/", ses.size)
check(await statusOf("GET", `/api/sessions/${ses.id}/events`, null, other.id) === 404, "session events are invisible from another workspace")

// accept, then undo the acceptance and accept again
await call("POST", "/api/decisions/" + acc.id, { choice: "pass" })
check((await call("GET", "/api/items/" + item.id)).status === "done", "accepted → done")
await call("POST", `/api/decisions/${acc.id}/undo`)
const back = await call("GET", "/api/items/" + item.id)
check(back.status === "wait" && back.open.includes(acc.id), "undo reopens the acceptance")
await call("POST", "/api/decisions/" + acc.id, { choice: "pass" })
const done = await call("GET", "/api/items/" + item.id); log("final:", done.status, done.stage, done.now)
const state = await call("GET", "/api/state")
check(done.status === "done" && !state.decisions.some((d) => d.item_id === item.id), "done, nothing left open")
console.log(JSON.stringify({ ok: true, item: item.id, kinds, plan: d1.plan, artifacts: d1.artifacts.length }))
rmSync(dir, { recursive: true, force: true })
