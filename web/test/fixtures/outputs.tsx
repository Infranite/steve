import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { Toaster } from "@/components/ui/toaster"
import { api, type RAnnotation, type RItem, type RItemDetail, type ROutPreview, type State } from "../../src/api"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import "../../src/manage"
import { refresh, useStore } from "../../src/store"
import { ItemView } from "../../src/views"

/* the item page of ST-9 against a fake Hub kept in localStorage, so a reload finds the drafts and sent notes where the
 * real Hub keeps them. Raw files (pictures, Office files) are served by the test; every other /api call fails. */
document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
const ID = "ST-9", T1 = Date.parse("2026-11-04T10:00:00+08:00"), T2 = T1 + 3_600_000
const design = new URLSearchParams(location.search).has("design")
const designDecision = { id: "design-1", kind: "ask" as const, subject_type: "design_implementation", subject_id: "{}", item_id: ID, session_id: null, question: "设计报告已交付，下一步？", draft: "请先查看产出。", options: ["按方案实现", "只保留设计", "修改设计"], status: "open" as const, result: null, requested_by: "dev", created: T2, resolved: null, undo: null }
const art = (id: string, kind: string, title: string, location: string | null, created: number) => ({ id, kind, title, location, created })
const artifacts = [
  art("a_branch1", "分支", "steve/st-9", null, T1), art("a_shots", "文件", "自查截图", "/tmp/st9/shots", T1 + 1), art("a_plan1", "文档", "方案", "docs/plan.md", T1 + 2),
  art("a_branch", "分支", "steve/st-9", null, T2), art("a_plan", "文档", "方案", "docs/plan.md", T2 + 1),
  art("a_img1", "图片", "首页.png", "/tmp/st9/out/home.png", T2 + 2), art("a_img2", "图片", "详情.png", "/tmp/st9/out/detail.png", T2 + 3), art("a_img3", "图片", "设置.png", "/tmp/st9/out/settings.png", T2 + 4),
  art("a_docx", "文档", "说明.docx", "/tmp/st9/out/说明.docx", T2 + 5), art("a_xlsx", "表格", "数据.xlsx", "/tmp/st9/out/数据.xlsx", T2 + 6), art("a_pptx", "演示", "汇报.pptx", "/tmp/st9/out/汇报.pptx", T2 + 7),
  art("a_rec", "文件", "截图", "/tmp/st9/shots2", T2 + 8),
]
const roles = { a_rec: "record" }
const file = (type: string, name: string, path: string): ROutPreview => ({ type, name, path, mime: "", large: false, size: 2048 } as ROutPreview)
const plan = "# 方案\n\n第一段说明：产出区只展示最终交付。\n\n## 细节\n\n- 多图左右切换\n- 文档选区批注\n"
const previews: Record<string, ROutPreview> = {
  a_plan: { type: "text", name: "plan.md", path: "docs/plan.md", lang: "markdown", text: plan, truncated: false, size: plan.length },
  a_plan1: { type: "text", name: "plan.md", path: "docs/plan.md", lang: "markdown", text: "# 方案\n\n旧版本", truncated: false },
  a_img1: file("image", "home.png", "/tmp/st9/out/home.png"), a_img2: file("image", "detail.png", "/tmp/st9/out/detail.png"), a_img3: file("image", "settings.png", "/tmp/st9/out/settings.png"),
  a_docx: file("office", "说明.docx", "/tmp/st9/out/说明.docx"), a_xlsx: file("office", "数据.xlsx", "/tmp/st9/out/数据.xlsx"), a_pptx: file("office", "汇报.pptx", "/tmp/st9/out/汇报.pptx"),
  a_rec: { type: "dir", path: "/tmp/st9/shots2", truncated: false, entries: ["d1.png", "d2.png"].map((name) => ({ name, dir: false, size: 2048, modified: null, form: "image" as const })) },
  "a_rec/d1.png": file("image", "d1.png", "/tmp/st9/shots2/d1.png"), "a_rec/d2.png": file("image", "d2.png", "/tmp/st9/shots2/d2.png"),
}

/* ── the fake Hub ── */
type Hub = { anns: RAnnotation[]; timeline: RItemDetail["timeline"]; keys: string[] }
const load = (): Hub => JSON.parse(localStorage.getItem("fx-hub") ?? "null") ?? { anns: [], timeline: [{ id: 1, ts: T1 - 1000, actor: "steve", kind: "sys", text: "建立", session_id: null }], keys: [] }
const save = (h: Hub) => localStorage.setItem("fx-hub", JSON.stringify(h))
const calls: unknown[] = ((window as any).__calls = [])
const wait = (ms: number) => new Promise((r) => setTimeout(r, ms))
const item: RItem = { id: ID, workspace_id: "default", no: 9, title: "产出预览与批注", status: "wait", stage: "交付与验收", bot_id: "dev", project_id: null, cwd: null, branch: "steve/st-9", plan: [], now: "交付了，等你验收", accept_mode: null, priority: "中", archived: false, star: false, due: null, snooze: null,
  source: ["对话", "chat"], parent: null, created: T1 - 1000, updated: T2, goal: "产出预览与批注", goals: 1, tags: [], deps: [], sessionIds: [], open: design ? [designDecision.id] : [], queued: 0, live: false }
const state = { version: "fixture", web_version: "fixture", workspace: { id: "default", name: "默认", description: "", item_prefix: "ST", sort: 0, created: 0 }, workspaces: [], items: [item], decisions: design ? [designDecision] : [], recent: [],
  bots: [{ id: "dev", name: "开发", role: "", main: false, harness: "claude-code", model: null, rules: "", persona: "", proactive: 1, accept_mode: "manual", perm_mode: "default", avatar: {}, default_project: null, prompt: null, device: null, enabled: true, revision: 1, updated: 0, workspaces: ["default"], memories: 0 }],
  allBots: [], projects: [], tags: [], tagUses: {}, thread: [], settings: {}, sessions: [], devices: [], runtimes: [] } as unknown as State
api.state = async () => state
api.item = async () => { const h = load(); return { ...item, goalsList: [{ version: 1, text: item.goal, created: T1 }], planRounds: [], timeline: h.timeline, artifacts, artifactRoles: roles, annotations: h.anns, decisions: design ? [designDecision] : [], dependents: [], thread: null } }
api.decide = async (_id, choice, text) => { calls.push({ choice, text }); return { ...designDecision, status: "resolved", result: { answer: choice, text }, resolved: Date.now() } }
api.events = async () => []
api.outPreview = async (_i, a, path) => { await wait(30); const p = previews[path ? `${a}/${path}` : a]; if (!p) throw new Error("没有这个产出"); return p }
let n = 0
api.annAdd = async (_i, p) => {
  const h = load(), a: RAnnotation = { id: `ann_${Date.now()}_${n++}`, item_id: ID, artifact_id: p.art, path: p.path ?? "", anchor: p.anchor, note: p.note, status: "draft", send_key: null, timeline_id: null, created: Date.now(), updated: Date.now(), sent: null }
  h.anns.push(a); save(h); calls.push({ add: p }); return a
}
api.annEdit = async (_i, id, note) => { const h = load(), a = h.anns.find((x) => x.id === id && x.status === "draft"); if (!a) throw new Error("这条批注已经发送，不能再改"); a.note = note; save(h); return a }
api.annRemove = async (_i, id) => { const h = load(), k = h.anns.findIndex((x) => x.id === id && x.status === "draft"); if (k < 0) throw new Error("这条批注已经发送，不能删除"); h.anns.splice(k, 1); save(h); return { ok: true } }
/* slow, like a Hub that also has to reach the agent: a second click lands while the first is still out */
api.annSend = async (_i, ids, key) => {
  calls.push({ send: ids, key })
  await wait(400)
  const h = load()
  if (h.keys.includes(key)) return { ok: true, again: true, sent: ids.length }
  const taken = h.anns.filter((a) => ids.includes(a.id) && a.status === "draft")
  if (!taken.length) throw new Error("这些批注已经发送过，或者已经删除")
  const line = Math.max(...h.timeline.map((t) => t.id)) + 1
  h.timeline.push({ id: line, ts: Date.now(), actor: "me", kind: "me", text: `对产出的批注（${taken.length} 条）：…`, session_id: null })
  for (const a of taken) Object.assign(a, { status: "sent", send_key: key, timeline_id: line, sent: Date.now() })
  h.keys.push(key); save(h)
  return { ok: true, again: false, sent: taken.length, timeline_id: line }
}
void refresh()
function App() { useStore(); return <SidebarProvider><main className="w-full"><ItemView id={ID} /></main><Toaster /></SidebarProvider> }
createRoot(document.getElementById("root")!).render(<App />)
