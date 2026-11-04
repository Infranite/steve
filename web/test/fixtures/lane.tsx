import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { Toaster } from "sonner"
import { api, type RLane, type RProject } from "../../src/api"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import "../../src/manage"
import { ProjectView } from "../../src/projects"
import { st, useStore } from "../../src/store"
document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
const p: RProject = { id: "prj_1", workspace_id: "default", name: "Steve", description: "Steve 自己的代码", device_id: "hub", path: "/Users/example/Agents/steve", kind: "git", worktree: true, default_bot: null, release: { kind: "script", cmd: "/Users/example/.steve/current/ops/release.sh", done_when: "tag" }, archived: false, created: 0, updated: 0 }
st.ready = true
st.projects = [p]
st.devices = [{ id: "hub", name: "Hub" } as (typeof st.devices)[number]]
/* what the Hub answered for the Steve repository on 2026-11-05, and a host that couldn't be read */
const steve: RLane = {
  target: { upstream: "github.com/infranite/steve", branch: "main" }, reads: "origin/main 8691b19", fetch_error: null,
  merge: { value: "push", how: { state: "found" }, evidence: ["GitHub：main 没有要求 PR，也没有合并队列"] },
  checks: { value: [{ name: "cargo test --manifest-path hub-rs/Cargo.toml --workspace", from: "suggested" }, { name: "npm --prefix web run check", from: "suggested" }], how: { state: "default" }, evidence: ["合并前没有托管方的检查：集成成功就算可合并", "hub-rs/Cargo.toml 是 Rust 工作区", "web/package.json 有 check 脚本"] },
  ship: { value: "tag", how: { state: "found" }, evidence: ["项目设置：完成标准是 PR 合并并推送 tag", ".github/workflows/release.yml 在推送 tag 时运行"] },
  numbered: [
    { kind: "version", path: "tag vX.Y.Z", evidence: "最新 v0.2.26，共 27 个", handling: "发布时由 ops/cut-release.py 取下一个号" },
    { kind: "notes", path: "docs/releases/", evidence: "25 份按版本写的说明", handling: "片段写在 docs/releases/unreleased/，发布时拼成一份" },
    { kind: "migrations", path: "hub-rs/crates/steve-store/migrations/", evidence: "31 个，最新 030.sql，有 LOCK", handling: "合并时取下一个空号" },
    { kind: "lockfile", path: "hub-rs/Cargo.lock", evidence: "依赖锁文件", handling: "冲突时用工具重新生成，不手改" },
  ],
  pace: { days: 7, landed: 46, releases: 27, authors: 1 },
}
const down: RLane = { ...steve, fetch_error: "fatal: unable to access 'https://github.com/infranite/steve/'", merge: { value: "push", how: { state: "unknown", why: "读不到 GitHub 上 main 的规则：gh: Bad credentials (HTTP 401)" }, evidence: [] }, checks: { ...steve.checks, how: { state: "unknown", why: "读不到 GitHub 上 main 的规则：gh: Bad credentials (HTTP 401)" }, evidence: steve.checks.evidence.slice(1) } }
api.lane = async () => (location.hash === "#down" ? down : steve)
function App() { useStore(); return <SidebarProvider><main className="w-full"><ProjectView id="prj_1" /></main><Toaster /></SidebarProvider> }
createRoot(document.getElementById("root")!).render(<App />)
