import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { api } from "../../src/api"
import { modelSetupApi } from "../../src/api/models"
import { Onboarding } from "../../src/onboarding"
import { st, useStore, meeting, setRuntimes } from "../../src/store"

/* first-run tour: ?member for someone who is not the owner, ?bare for a machine without runtimes or projects */
document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
const q = location.search, now = Date.now()
const calls: string[] = []
window.__calls = calls
api.onboardStart = async () => { calls.push("start"); st.settings = { ...st.settings, onboard: "pending" }; return { message: "hello" } }
api.onboardSkip = async () => { calls.push("skip"); st.settings = { ...st.settings, onboard: "done" }; return undefined }
st.owner = !q.includes("member")
st.workspace = { id: "w1", name: "个人", description: "", item_prefix: "ST", sort: 0, created: now }
const rt = (id: string, name: string, ok: boolean, models: string[], extra = {}) => ({ id, name, ok, version: "", path: "", models, note: "通过 ACP 适配器运行", ...extra })
const runtimes = q.includes("bare") ? [] : [rt("claude-code", "Claude Code", true, ["opus", "sonnet", "haiku"]), rt("codex", "Codex", true, []), rt("gemini", "Gemini CLI", false, [], { error: "没有找到 gemini 命令", install_hint: "@google/gemini-cli" })]
setRuntimes(runtimes)
let view = {
  steve: { harness: "claude-code", model: null as string | null, models: [] as string[] }, bot_revision: 1,
  harnesses: runtimes.map((r) => ({ id: r.id, name: r.name, ready: r.ok, judgments: true })),
  purposes: [{ key: "route", label: "分流", follows: false, effective: { harness: "claude-code", models: ["haiku"] } }],
}
modelSetupApi.get = async () => structuredClone(view)
modelSetupApi.select = async (c) => { calls.push("model:" + c.harness); view = { ...view, bot_revision: view.bot_revision + 1, steve: { harness: c.harness, model: c.model, models: c.model ? [c.model] : [] } }; return structuredClone(view) }
const proj = (id: string, name: string, path: string, archived = false) => ({ id, workspace_id: "w1", name, description: "", device_id: "hub", path, kind: "git" as const, worktree: true, default_bot: null, release: null, archived, created: now, updated: now })
st.projects = q.includes("bare") ? [] : [proj("p1", "Steve", "/home/example/Agents/clients/a-rather-long-client-folder/steve-dev-loop"), proj("p2", "website", "~/Sites/website"), proj("p3", "notes", "~/notes"), proj("p4", "infra", "~/infra"), proj("p5", "scratch", "~/scratch"), proj("p6", "old", "~/old", true)]
st.onboard = 0
function Fixture() {
  useStore()
  return <main className="mx-auto flex max-w-[720px] flex-col gap-4 p-6" data-fixture-state={st.onboard} data-meeting={meeting() ? "" : undefined}><Onboarding /></main>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
