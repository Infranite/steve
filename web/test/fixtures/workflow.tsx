import { useState } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { DependencyWaitPanel } from "../../src/dependency-wait-panel"
import { WorkflowPanel } from "../../src/workflow-panel"
import { RelationGraph } from "../../src/graph"
import { st, type Item } from "../../src/store"
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
    { ...unit("api", "实现接口与主体检查", "work", "succeeded"), summary: "接口已实现，检查通过。", outputs: [{ kind: "文件", title: "工作图 API", location: "src/workflow.ts", role: "result" }, { kind: "日志", title: "构建记录", role: "record" }], checks: [{ name: "接口实际回读", passed: true, operated: true }] },
    unit("web", "实现协作进度界面", "work", "running"),
    { ...unit("review", "独立复核依赖门控与恢复", "review", "failed", ["api", "web"]), reason: "刷新恢复后需要补充验证。" },
  ],
  blockers: ["独立复核依赖门控与恢复尚未通过", "集成并统一验收尚未通过"],
  history: [{ revision: 1, reason: "拆分为接口、界面、独立评审与统一验收。", created: 1 }, { revision: 7, reason: "根据独立评审结果补充恢复验证。", created: 2 }],
}

function Fixture() {
  const query = new URLSearchParams(location.search)
  const [workflow, setWorkflow] = useState<Workflow>({ ...initial, current_cycle: query.has("prior") ? false : undefined,
    units: initial.units.map((u) => query.has("rework") && u.id === "web" ? { ...u, status: "failed" } : query.has("reworkActive") && u.id === "review" ? { ...u, status: "running" } : u),
    history_attempts: query.has("archive") ? [{ item_id: "ST-old", unit_id: "api", created: 1, snapshot: { title: "旧接口尝试", status: "cancelled", attempt: 1, session_id: "old-session", reason: "已被返工替代" } }] : [],
    history_attempts_total: query.has("archive") ? 2 : 0 })
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
    if (args.action === "single_session" && query.has("conflict")) throw new Error("工作图已变化，请刷新后重新确认验收方式")
    const single = args.action === "single_session"
    const next: Workflow = {
      ...workflow, revision: workflow.revision + 1,
      acceptance_mode: single ? "single_session" : workflow.acceptance_mode,
      blockers: single ? [] : workflow.blockers,
      history: single ? [...workflow.history, { revision: workflow.revision + 1, reason: args.reason, created: 3, action: args.action }] : workflow.history,
      paused: args.action === "pause" ? true : args.action === "resume" ? false : workflow.paused,
      units: workflow.units.map((u) => single && u.status !== "succeeded" ? { ...u, status: "cancelled" } : args.action === "retry" && u.id === args.unit_id ? { ...u, status: "pending", reason: "", attempt: u.attempt + 1 } : u),
    }
    ;(window as any).__workflowChanges ??= []
    ;(window as any).__workflowChanges.push(args)
    setWorkflow(next)
    return next
  }
  if (query.has("wait")) return <main className="mx-auto max-w-3xl p-5"><DependencyWaitPanel item="ST-60" wait={wait} /></main>
  if (query.has("graph")) {
    st.items = [
      { id: "ST-60", t: "主事项", st: "run", b: "steve", project: "project", deps: ["ST-missing"], workflow },
      { id: "ST-61", t: "实现接口", st: "run", b: "steve", project: "project", deps: [], parent: { item: "ST-60", s: "session-old", at: 23 }, arch: true },
    ] as Item[]
    st.S = { "session-old": { title: "设计来源会话", st: "done", b: "steve", n: 1 } } as typeof st.S
    st.projects = [{ id: "project", name: "示例项目" }] as typeof st.projects
    workflow.units[1].item_id = "ST-61"
    return <main className="mx-auto max-w-4xl p-5"><RelationGraph focus="ST-60" scope="comp" onOpen={(n) => { (window as any).__graphOpened = n.id }} /></main>
  }
  return <main className="mx-auto max-w-3xl p-5"><WorkflowPanel item="ST-60" workflow={workflow} /></main>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
