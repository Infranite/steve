// Acceptance (restart): the agent asks a question, the Hub restarts while it waits, the answer after the restart
// resumes the same ACP session (session/load) and the item finishes. The caller passes the command that restarts the Hub.
// Usage: node test/e2e-restart.mjs <base-url> <restart-command>   (see test/e2e.sh)
import { execSync } from "node:child_process"
import { readFileSync, existsSync, rmSync } from "node:fs"
import { join } from "node:path"
import { base, login, call, sleep, log, waitFor, openOf, check, setup, handle } from "./lib.mjs"
const restart = process.argv[3]; if (!restart) throw new Error("usage: e2e-restart.mjs <base-url> <restart-command>")

await login()
const { dir, project, bot } = await setup("restart")
const item = await call("POST", "/api/items", { goal: "先用 steve_ask 问我：要写 alpha 还是 beta？选项就是这两个。拿到回答后，用 Write 工具（不要用 shell 命令）在当前目录新建 choice.txt，只写入我选的那个词，然后用 steve_deliver 交付。", title: "问一个选择", bot: bot.id, project: project.id })
log("item", item.id)
const ask = await waitFor(async () => (await openOf(item.id)).find((d) => d.kind === "ask"), "question")
log("asked:", ask.question, ask.options)
const s0 = (await call("GET", "/api/items/" + item.id)).sessionIds[0], acp0 = (await call("GET", "/api/sessions/" + s0)).acp_session_id
await waitFor(async () => !(await call("GET", "/api/sessions/" + s0)).busy, "turn to end after asking", 60000)
await sleep(1000)

log("restarting hub…"); execSync(restart, { stdio: "inherit" })
await waitFor(async () => (await fetch(base + "/healthz")).ok, "hub back", 60000)
await login()
log("hub is back")
check((await openOf(item.id)).some((d) => d.id === ask.id), "question survived the restart")
await call("POST", "/api/decisions/" + ask.id, { choice: "beta" }); log("answered beta")

let accept
while (!accept) {
  const ds = await waitFor(async () => { const x = await openOf(item.id); return x.length ? x : null }, "next decision")
  for (const d of ds) accept = (await handle(d, dir)) ?? accept
}
log("delivered:", accept.draft.slice(0, 100))
await call("POST", "/api/decisions/" + accept.id, { choice: "pass" })
const fin = await call("GET", "/api/items/" + item.id)
const ses = await call("GET", "/api/sessions/" + fin.sessionIds[0])
const sys = (await call("GET", `/api/sessions/${ses.id}/events`)).filter((e) => e.kind === "system").map((e) => e.data.text)
const content = existsSync(join(dir, "choice.txt")) ? readFileSync(join(dir, "choice.txt"), "utf8").trim() : ""
check(fin.sessionIds.length === 1 && ses.acp_session_id === acp0, "the same ACP session continued after the restart")
check(fin.status === "done" && content === "beta", "item done, choice.txt = beta")
console.log(JSON.stringify({ ok: true, status: fin.status, content, sessions: fin.sessionIds.length, system: sys }))
rmSync(dir, { recursive: true, force: true })
