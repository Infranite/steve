import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { Toaster } from "sonner"
import { api, type RProject } from "../../src/api"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import "../../src/manage"
import { ProjectView } from "../../src/projects"
import { st, useStore } from "../../src/store"
document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
const p: RProject = { id: "prj_1", workspace_id: "default", name: "Steve", description: "Steve 自己的代码：Hub、Web 界面、发布与运维脚本", device_id: "hub", path: "/Users/example/Agents/steve", kind: "git", worktree: true, default_bot: null, release: { kind: "script", cmd: "/Users/example/.steve/current/ops/release.sh" }, archived: false, created: 0, updated: 0 }
st.ready = true
st.projects = [p]
st.devices = [{ id: "hub", name: "Hub" } as (typeof st.devices)[number]]
/* the Hub: PATCH /projects/:id answers with the project; the calls are kept for the test */
const calls: unknown[] = ((window as any).__calls = [])
api.patchProject = async (id, body) => {
  calls.push({ id, body })
  const { done_when, ...rest } = body, cur = st.projects[0]
  /* like the Hub: `done_when` goes into the release settings, the command stays */
  const release = done_when === undefined || !cur.release ? cur.release : { kind: cur.release.kind, cmd: cur.release.cmd, ...(done_when ? { done_when } : {}) }
  st.projects = [{ ...cur, ...rest, release, updated: Date.now() }]
  return st.projects[0]
}
function App() { useStore(); return <SidebarProvider><main className="w-full"><ProjectView id="prj_1" /></main><Toaster /></SidebarProvider> }
createRoot(document.getElementById("root")!).render(<App />)
