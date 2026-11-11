import { useState } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { DependencyWaitPanel } from "../../src/dependency-wait-panel"
import { WorkflowPanel } from "../../src/workflow-panel"
import type { Workflow, WorkflowUnit } from "../../src/workflow"
import { api, type RItemDetail, type DependencyWait } from "../../src/api"
import { setLang } from "../../src/i18n"

setLang("zh-CN")
document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
const unit = (id: string, title: string, kind: WorkflowUnit["kind"], status: string, depends_on: string[] = []): WorkflowUnit => ({
  id, title, goal: "按主事项的已确认目标执行，交付可复核的结果及检查记录。",
  kind, status, depends_on, resources: ["shared/contract"], required: true, bot_id: kind === "review" ? "验收 Bot" : "开发 Bot",
  attempt: status === "pending" ? 0 : 1, summary: "", outputs: [], checks: [], reason: "",
})
const initial: Workflow = {
  item_id: "ST-60", revision: 7, paused: new URLSearchParams(location.search).has("unavailable"), execution_ready: !new URLSearchParams(location.search).has("unavailable"), max_parallel: 2, max_attempts: 3,
  units: [
    unit("integrate", "集成并统一验收", "integrate", "pending", ["review"]),
    { ...unit("api", "实现接口与主体检查", "work", "succeeded"), summary: "接口已实现，检查通过。", outputs: [{ kind: "文件", title: "工作图 API", location: "src/workflow.ts" }], checks: [{ name: "接口实际回读", passed: true, operated: true }] },
    unit("web", "实现协作进度界面", "work", "running"),
    { ...unit("review", "独立复核依赖门控与恢复", "review", "failed", ["api", "web"]), reason: "刷新恢复后需要补充验证。" },
  ],
  blockers: ["独立复核依赖门控与恢复尚未通过", "集成并统一验收尚未通过"],
  history: [{ revision: 1, reason: "拆分为接口、界面、独立评审与统一验收。", created: 1 }, { revision: 7, reason: "根据独立评审结果补充恢复验证。", created: 2 }],
}

function Fixture() {
  const [workflow, setWorkflow] = useState(initial)
  const query = new URLSearchParams(location.search)
  const [wait, setWait] = useState<DependencyWait>({
    item_id: "ST-60", status: "block", phase: "active", cycle: 11,
    dependencies: [{ id: "ST-59", status: "run", phase: "active" }], open_dependencies: ["ST-59"],
    resume_mode: "automatic", resume_source: "workspace", resume_override: "workspace",
    resume_enabled: !query.has("held"), resume_blocker: query.has("held") ? "实例暂时暂停调度" : null,
    resume_fallback_reason: null, waiting: false, wait_state: null, reason: null, active_turn: false,
    can_register: true, register_blocker: null,
  })
  api.saveDependencyWait = async (_id, args) => {
    ;(window as any).__waitChanges ??= []
    ;(window as any).__waitChanges.push(args)
    if (query.has("stale")) throw new Error("事项已进入另一轮，请刷新后再设置等待")
    const next: DependencyWait = { ...wait, waiting: true, wait_state: "waiting", reason: args.reason,
      resume_override: args.resume, resume_source: args.resume === "workspace" ? "workspace" : "item",
      resume_mode: args.resume === "notify" ? "notify" : "automatic" }
    setWait(next)
    return next
  }
  // The panel refreshes after an action. Keep fixture interactions off every real Hub.
  api.item = async () => ({} as RItemDetail)
  api.workflowChange = async (_id, args) => {
    const next = {
      ...workflow, revision: workflow.revision + 1,
      paused: args.action === "pause" ? true : args.action === "resume" ? false : workflow.paused,
      units: workflow.units.map((u) => args.action === "retry" && u.id === args.unit_id ? { ...u, status: "pending", reason: "", attempt: u.attempt + 1 } : u),
    }
    ;(window as any).__workflowChanges ??= []
    ;(window as any).__workflowChanges.push(args)
    setWorkflow(next)
    return next
  }
  if (query.has("wait")) return <main className="mx-auto max-w-3xl p-5"><DependencyWaitPanel item="ST-60" wait={wait} /></main>
  return <main className="mx-auto max-w-3xl p-5"><WorkflowPanel item="ST-60" workflow={workflow} /></main>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
