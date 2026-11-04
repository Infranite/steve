/* 设置 · 统计夹具（ST-52）：真实的 StatsSettingsPane，价格接口由浏览器测试拦截。?theme=dark 切深色。 */
import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { StatsSettingsPane } from "../../src/stats/prices"

if (new URLSearchParams(location.search).get("theme") === "dark") document.documentElement.classList.add("dark")

createRoot(document.getElementById("root")!).render(
  <StrictMode><TooltipProvider delay={0}><main className="mx-auto max-w-3xl p-4 sm:p-8"><StatsSettingsPane /></main></TooltipProvider></StrictMode>,
)
