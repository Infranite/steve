/* the conversation of the chat-history fixture: 500 messages, a pause of two hours every 40 (a new topic), the last 200
 * in the page's window */
import type { MessageView, PageOf } from "../../src/gen/index.ts"
import { messageView, pageResponse } from "./wire-views.ts"

export const older = 300
export const BASE = new Date(2026, 9, 28, 9, 0).getTime()
export const created = (i: number) => BASE + i * 60_000 + Math.floor(i / 40) * 2 * 3600_000
export const text = (i: number) =>
  i === 50 ? "这里讨论定位目标的细节：点开历史结果后要跳到原文。"
  : i % 7 === 0 ? `第 ${i} 条，长一些：把找历史和切换当前聊天分开。翻昨天的消息只改变阅读位置，输入框默认仍给 Steve 发新消息；需要接着某句说，就引用它。`
  : `第 ${i} 条消息`

export function olderMessages(ws = "ws-history"): MessageView[] {
  return Array.from({ length: older }, (_, i) => messageView({
    id: "o" + i,
    ws,
    author: i % 2 ? "steve" : "me",
    text: text(i),
    item_id: i === 51 ? "ST-7" : null,
    created: created(i),
  }))
}

export function olderMessagePage(messages: MessageView[], before: number): PageOf<MessageView> {
  const all = messages.filter((m) => m.created < before).reverse()
  const views = all.slice(0, 100)
  const last = views.at(-1)
  return pageResponse(views, 0, all.length > views.length && last ? `${last.created}:${last.id}` : null)
}
