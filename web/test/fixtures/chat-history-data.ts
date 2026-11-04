/* the conversation of the chat-history fixture: 500 messages, a pause of two hours every 40 (a new topic), the last 200
 * in the page's window */
export const older = 300
export const BASE = new Date(2026, 9, 28, 9, 0).getTime()
export const created = (i: number) => BASE + i * 60_000 + Math.floor(i / 40) * 2 * 3600_000
export const text = (i: number) =>
  i === 50 ? "这里讨论定位目标的细节：点开历史结果后要跳到原文。"
  : i % 7 === 0 ? `第 ${i} 条，长一些：把找历史和切换当前聊天分开。翻昨天的消息只改变阅读位置，输入框默认仍给 Steve 发新消息；需要接着某句说，就引用它。`
  : `第 ${i} 条消息`
