import { StrictMode, useEffect, useState } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { ChannelMessageBody } from "../../src/channels/message-body"
import type { ChanMessage } from "../../src/api/channels"
import type { CardAttachment } from "../../src/api/channel-content"

const card1: CardAttachment = { type: "card", schema: 1, format: "schema", title: "发布评审", subtitle: "研发群 · 周三", template: "blue", tags: ["待确认"], truncated: false, blocks: [
  { t: "markdown", text: "**版本 1.0 卡片**\n\n已完成回归，请查看发布说明。" },
  { t: "columns", columns: [[{ t: "markdown", text: "负责人\n\n@小林" }], [{ t: "markdown", text: "计划时间\n\n今天 18:00" }]] },
  { t: "hr" }, { t: "image", alt: "架构示意图" },
  { t: "buttons", items: [{ text: "查看说明", url: "https://example.com/release", disabled: false, callback: false }, { text: "批准发布", disabled: false, callback: true }, { text: "已停用操作", url: "https://example.com/disabled", disabled: true, callback: false }, { text: "恶意操作", url: "javascript:alert(1)", disabled: false, callback: false }] },
  { t: "note", text: "审批操作请返回飞书完成。" },
] }
const card2: CardAttachment = { type: "card", schema: 2, format: "user_dsl", title: "项目进展", template: "green", truncated: true, blocks: [
  { t: "heading", level: 3, text: "版本 2.0 卡片" },
  { t: "table", columns: ["任务", "进度", "说明"], rows: [["读取", "完成", "**1.0 / 2.0**"], ["渲染", "验收中", "长文件名_".repeat(20)]] },
  { t: "panel", title: "详细信息", expanded: false, blocks: [{ t: "markdown", text: "折叠区正文" }] },
  { t: "control", kind: "input", text: "请填写你的意见" },
  { t: "chart", title: "趋势", summary: "本周 12 项已完成" },
  { t: "group", kind: "form", blocks: [{ t: "markdown", text: "表单说明" }] },
  { t: "unknown", tag: "future-widget", text: "新组件保留的文字" },
] }
const msg = (id: string, card: CardAttachment): ChanMessage => ({ id, space_id: "space", direction: "in", sender: { name: "同事", party_id: null }, persona_id: null, on_behalf: "none", text: "卡片文本回退", attachments: [card], status: "received", work_item_id: null, decision_id: null, created: 1, sent: null, purged: null, basis: null })
function Fixture() {
  const [one, setOne] = useState(msg("card_one", card1)), [two, setTwo] = useState(msg("card_two", card2))
  useEffect(() => {
    const purge = () => setOne(m => ({ ...m, text: null, purged: 2 }))
    const recall = () => setTwo(m => ({ ...m, status: "recalled" }))
    window.addEventListener("fixture-purge", purge); window.addEventListener("fixture-recall", recall)
    return () => { window.removeEventListener("fixture-purge", purge); window.removeEventListener("fixture-recall", recall) }
  }, [])
  return <main style={{ maxWidth: 720, margin: "auto", padding: 20 }} className="flex flex-col gap-5">
    <ChannelMessageBody message={one} /><ChannelMessageBody message={two} />
    <ChannelMessageBody message={{ ...msg("image", card1), text: "", attachments: [{ type: "image", url: "https://tracking.example.test/evil.png", name: "非法资源地址" }] }} />
    <ChannelMessageBody message={{ ...msg("folder", card1), text: "", attachments: [{ type: "file", kind: "folder", name: "项目资料", unavailable: "folder" }] }} />
  </main>
}
createRoot(document.getElementById("root")!).render(<StrictMode><Fixture /></StrictMode>)
