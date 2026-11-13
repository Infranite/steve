import { StrictMode, useState } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { TooltipProvider } from "../../src/components/ui/tooltip"
import { Composer } from "../../src/composer"
import { QueueStrip } from "../../src/queue"
import { Segmented } from "../../src/ui"
import type { RQueued } from "../../src/api"

/* The three composers as their pages set them up: a session whose turn runs (Enter steers, ⌥ Enter queues, Stop and
 * Esc interrupt), the conversation while Steve answers (what is sent queues), and an item's idle composer with its
 * own toggle. Every send, stop and command lands in window.log. */
const q = new URLSearchParams(location.search)
document.documentElement.classList.toggle("dark", q.has("dark"))
declare global { interface Window { log: string[] } }
window.log = []
const log = (s: string) => { window.log.push(s) }
const row = (id: string, text: string): RQueued => ({ id, via: "me", text, created: 1, files: [] })
function Fixture() {
  const [running, setRunning] = useState(true), [target, setTarget] = useState("auto")
  return (
    <TooltipProvider>
      <main className="mx-auto flex w-full max-w-3xl flex-col gap-10 p-4 sm:p-8">
        <section data-fixture="session">
          <button type="button" onClick={() => setRunning((r) => !r)} data-toggle-run>toggle</button>
          <Composer testId="session" placeholder="直接和 agent 对话…" model="Claude Code · claude-opus-5-5" busy={running} steers
            onStop={() => log("stop")} onSend={(t, _f, _r, _m, _id, how) => { log(`send:${how ?? "idle"}:${t}`) }}
            queue={<QueueStrip sid="s1" rows={[row("q1", "先跑一遍测试"), row("q2", "再看看移动端的布局，尤其是 390 宽的时候队列里的按钮会不会挤在一起")]} note="本回合结束后发送" />}
            context={{ value: 0.86, title: "上下文已用 86%" }} />
        </section>
        <section data-fixture="chat">
          <Composer testId="chat" placeholder="和 Steve 说…" busy onStop={() => log("chat-stop")} onSend={(t, _f, _r, _m, _id, how) => { log(`chat:${how ?? "idle"}:${t}`) }} onCommand={(c) => log("cmd:" + c)} />
        </section>
        <section data-fixture="item">
          <Composer testId="item" placeholder="补充给 ST-1… 输入 # 引用其他事项" onSend={(t) => { log("item:" + t) }}
            left={<Segmented size="sm" value={target} onChange={setTarget} options={[["auto", "自动排队"], ["new", "新会话"]] as const} />} />
        </section>
      </main>
    </TooltipProvider>
  )
}
createRoot(document.getElementById("root")!).render(<StrictMode><Fixture /></StrictMode>)
