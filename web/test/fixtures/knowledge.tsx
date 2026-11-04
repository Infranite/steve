import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { Toaster } from "sonner"
import { KnowledgeView } from "../../src/knowledge"
import { MemConfirm, memAsk } from "../../src/knowledge/parts"
import { Card, CardHeader, CardTitle } from "../../src/components/ui/card"
import { SidebarInset, SidebarProvider } from "../../src/components/ui/sidebar"
document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
/* the 待处理 card of a candidate (ST-41 wording), next to one opened before ST-41 */
const cards = [
  { q: "存为长期记忆吗？主会话收到需求时直接给出方案，不再每次询问是在当前会话出方案还是创建卡片。", draft: "类型：长期记忆 · 偏好\n为什么值得保留：以后每个需求都适用，省去每次确认出方案还是建卡片\n适用：主会话收到需求时\n来源：ST-33 交付时\n原话：主会话就不用每次都问在这出方案还是创建卡片了，直接出方案就行。" },
  { q: "记住这条吗？用户希望卡片详情中的排队信息只在评论区展示，不在输入框顶部重复显示。", draft: "来源：ST-39 交付时\n原话：卡片详情这个ui有问题，重复展示了" },
]
const Inbox = () => <div className="flex flex-col gap-3" data-fixture-cards>{cards.map((c, n) => (
  <Card key={n} className="gap-0 py-0" data-fixture-card={n}><CardHeader className="pt-4 pb-2"><CardTitle className="type-headline" data-card-ask>{memAsk(c.q)}</CardTitle></CardHeader>
    <MemConfirm q={c.q} draft={c.draft} busy={false} act={() => {}} /></Card>))}</div>
createRoot(document.getElementById("root")!).render(location.search.includes("inbox")
  ? <main className="mx-auto max-w-[640px] px-4 py-6"><Inbox /></main>
    /* the app's shell around a view (App.tsx) */
  : <SidebarProvider><SidebarInset className="relative min-w-0 overflow-hidden"><div className="min-h-0 flex-1"><KnowledgeView /></div></SidebarInset><Toaster /></SidebarProvider>)
