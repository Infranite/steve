import { fixtureBot, fixtureItem, fixtureProject, fixtureWait, seedItems, seedSessions } from "./view-data"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { ItemRow } from "../../src/view-shared"
import { ItemsView } from "../../src/items-list"
import { SidebarInset, SidebarProvider } from "../../src/components/ui/sidebar"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { setLang } from "../../src/i18n"
import { resetAppearance, setAppearance } from "../../src/themes"
import { st, useStore, type Item, type Wait } from "../../src/store"

const q = new URLSearchParams(location.search)
document.documentElement.classList.toggle("dark", q.has("dark") || matchMedia("(prefers-color-scheme: dark)").matches)
document.documentElement.classList.toggle("font-large", q.has("large"))
if (q.has("sysmono")) setAppearance({ mono: "system-mono" }); else resetAppearance()
if (q.has("en")) void setLang("en")
const containerW = Number(q.get("container")) || 0

const now = Date.now()
const d0 = new Date(now); d0.setHours(0, 0, 0, 0)
const todayStart = d0.getTime()
const tsToday = now - todayStart >= 2 * 60_000 ? todayStart + 60_000 : now - 90_000
const tsYesterday = todayStart - 12 * 3_600_000 - 13 * 60_000
const tsWeekday = todayStart - 3 * 86_400_000 + 10 * 3_600_000
const tsOlder = todayStart - 10 * 86_400_000

const dev = fixtureBot({ id: "dev", n: "Steve 开发", h: "Codex", harness: "codex", m: "gpt-6.1-sol", model: "gpt-6.1-sol", color: "#8b9eac" })
const rev = fixtureBot({ id: "rev", n: "Steve 复核", h: "Claude Code", harness: "claude-code", m: "claude-opus-5-5", model: "claude-opus-5-5", color: "#3e63dd" })
const item = (id: string, t: string, x: Partial<Item>): Item => fixtureItem({ id, t, phase: "active", ws: "", up: "02:03", acc: "", created: now, updated: now, ...x })
const wait = (type: Wait["type"], q: string, subject = "action"): Wait => fixtureWait({ type, q, since: "3 分钟", decision: "d-" + q, subject })
const approve = wait("approve", "合并 PR #12 并打 tag 发版")
st.bots = { dev, rev }
st.projects = [
  fixtureProject({ id: "p1", name: "steve", default_bot: "dev" }),
  fixtureProject({ id: "p2", name: "long-project-workspace-name", default_bot: "rev" }),
]
st.tags = [
  { id: "t-bug", name: "bug", cat: "类型", desc: "", by: "Steve", created: "" },
  { id: "t-feat", name: "feat", cat: "类型", desc: "", by: "Steve", created: "" },
  { id: "t-refactor", name: "refactor", cat: "类型", desc: "", by: "Steve", created: "" },
  { id: "t-domain", name: "ui", cat: "领域", desc: "", by: "Steve", created: "" },
]
const rows = [
  /* a reopened cycle nobody has spoken to yet: it waits for your word, the summary says so */
  item("ST-49", "对话页左右滑动白线显示", { st: "wait", progress: "新周期，等你说要做什么", tags: ["t-bug"], project: "p1", updated: tsToday }),
  /* the same cycle once the bot took it up: running, its own plan */
  item("ST-48", "修复执行进度重复展示与计时不一致", { st: "run", progress: "新周期，收到新消息，开始处理", plan: [["查构建日志", 0]], tags: ["t-bug"], project: "p1", updated: tsToday }),
  /* a real wait: the card's question and the way to it, instead of the stale summary */
  item("ST-52", "发布脚本加上回滚", { st: "wait", progress: "交付了，等你验收", plan: [["改脚本", 1], ["测试", 1]], wait: approve, waits: [approve, wait("ask", "回滚保留几个版本？")], tags: ["t-feat"], project: "p1", updated: tsToday }),
  item("ST-53", "选择数据库迁移方案", { st: "wait", progress: "等你回答", wait: wait("ask", "迁移要在白天还是晚上做？这会影响要不要先停写入，一个很长的问题，看看会不会把行撑出去"), waits: [], tags: ["t-bug"], project: "p2", updated: tsYesterday }),
  item("ST-5", "修复权限状态", { progress: "正在检查边界", tags: ["t-bug"], project: "p1", updated: tsToday }),
  item("ST-10", "整理发布说明", { st: "pause", progress: "已暂停 · 等下周发版", tags: ["t-refactor"], project: "p1", plan: [["草拟说明", 1], ["核对提交", 0], ["发布检查", 0], ["确认归档", 0], ["收尾", 0], ["发布", 0]], updated: now }),
  item("ST-44", "下一周期继续", { cycle: 2, progress: "正在补齐测试", q: [{ id: "q1", text: "补充一", via: "me", created: now }, { id: "q2", text: "补充二", via: "me", created: now }], ss: ["s1", "s2"], parent: { item: "ST-1" }, tags: ["t-feat"], project: "p1", updated: tsToday }),
  item("ST-27", "另一项", { progress: "正在处理", updated: tsToday }),
  item("ST-28", "普通派生", { parent: { item: "ST-27" }, progress: "独立工作", updated: tsToday }),
  item("ST-9999", "修复大字号下编号折行并对齐右侧多列信息（含高优先级与截止时间）", { st: "run", b: "rev", pri: "高", due: "今天 18:00", star: true, tags: ["t-refactor", "t-domain"], project: "p2", plan: [["重构网格", 1], ["对齐列轨", 0], ["补全测试", 0]], progress: "正在核对 4 位数编号与多列宽度对齐情况", ss: ["s9999a", "s9999b", "s9999c"], updated: tsYesterday }),
  item("ST-239", "#665 修复测试脚本残留本地进程", { st: "run", b: "dev", tags: ["t-bug"], project: "p1", progress: "正在验证守护进程退出清理", ss: ["s239a"], updated: tsToday }),
  item("ST-235", "GitHub #651，Linux CI 上校验容器断点", { st: "done", phase: "closed", b: "dev", tags: [], project: null, progress: "已通过全部检查", ss: ["s235a"], updated: tsYesterday }),
  item("ST-231", "把升级演练做成一条命令并保留未完成步骤提示", { st: "done", phase: "closed", b: "rev", tags: ["t-bug"], project: "p1", plan: [["a", 1], ["b", 1], ["c", 1], ["d", 1], ["e", 0]], progress: "收尾待补文档", updated: tsWeekday }),
  item("ST-238", "已放弃的旧版流式排布实验方案", { st: "drop", phase: "closed", b: "dev", tags: ["t-feat"], project: null, plan: [["a", 0], ["b", 0], ["c", 0], ["d", 0]], progress: "改用 2D CSS Grid 替代", ss: ["s238a", "s238b"], updated: tsOlder }),
  /* closed as failed: a finished row in its own group, not lost from the list */
  item("ST-43", "独立审查回归覆盖", { st: "fail", phase: "closed", progress: "失败：评审不通过", tags: ["t-bug"], project: "p1", updated: tsWeekday }),
  item("ST-30", "已完成的独立事项", { st: "done", phase: "closed", progress: "已完成归档前展示", tags: ["t-feat"], project: "p1", updated: tsOlder }),
]
rows[3].waits = [rows[3].wait!]
seedItems(rows)
seedSessions({
  s44a: { item: "ST-44", b: "dev", h: "Codex", harness: "codex", m: "gpt-6.1-sol", st: "run", since: now - 5_000, at: now },
  s44b: { item: "ST-44", b: "rev", h: "Claude Code", harness: "claude-code", m: "claude-opus-5-5", st: "ended", since: now - 10_000, at: now - 5_000 },
  s9999a: { item: "ST-9999", b: "rev", h: "Claude Code", harness: "claude-code", m: "claude-opus-5-5", st: "run", since: now - 2_000, at: now },
  s9999b: { item: "ST-9999", b: "dev", h: "Codex", harness: "codex", m: "gpt-6.1-sol", st: "ended", since: now - 4_000, at: now - 2_000 },
  s9999c: { item: "ST-9999", b: "dev", h: "Codex", harness: "codex", m: "gpt-6.1-sol", st: "ended", since: now - 6_000, at: now - 4_000 },
  s239a: { item: "ST-239", b: "dev", h: "Codex", harness: "codex", m: "gpt-6.1-sol", st: "run", since: now - 3_000, at: now },
  s235a: { item: "ST-235", b: "dev", h: "Codex", harness: "codex", m: "gpt-6.1-sol", st: "ended", since: now - 3_000, at: tsYesterday },
  s238a: { item: "ST-238", b: "dev", h: "Codex", harness: "codex", m: "gpt-6.1-sol", st: "ended", since: now - 3_000, at: tsOlder },
  s238b: { item: "ST-238", b: "rev", h: "Claude Code", harness: "claude-code", m: "claude-opus-5-5", st: "ended", since: now - 6_000, at: tsOlder },
})
function Fixture() {
  useStore()
  return <main style={{ maxWidth: containerW || 880, margin: "auto", padding: 20 }} className="flex flex-col">{rows.map((i) => <ItemRow key={i.id} i={i} />)}</main>
}
createRoot(document.getElementById("root")!).render(
  <TooltipProvider>
    {q.has("groups") ? (
      <SidebarProvider>
        <SidebarInset className="relative min-w-0 overflow-hidden" style={containerW ? { maxWidth: containerW, marginInline: "auto" } : undefined}>
          <ItemsView />
        </SidebarInset>
      </SidebarProvider>
    ) : <Fixture />}
  </TooltipProvider>,
)
