/** Older Hubs store the handoff context as text. Recognize only the format emitted by
 * steve-channels/decide.rs; keep unfamiliar cards intact rather than guessing who spoke. */
export function handoffContent(question: string) {
  const [head, ...lines] = question.split("\n")
  const history = lines.indexOf("最近的对话：")
  if (history < 0) return null
  const excerpt = lines.slice(history + 1)
  const heading = /^(⭐ 星标联系人 · )?「(.+)」里 (.+) 找你：(.*)$/.exec(head)
  const last = excerpt.at(-1) ?? ""
  const prefix = heading && `→ ${heading[3]}：`
  const target = prefix && last.startsWith(prefix)
    ? [last, heading![3], last.slice(prefix.length)]
    : /^→ ([^：]+)：(.*)$/.exec(last)
  if (!target || excerpt.slice(0, -1).some((l) => !l.startsWith("- "))) return null
  return {
    question: heading?.[4] || head,
    space: heading?.[2],
    starred: !!heading?.[1],
    sender: target[1],
    text: target[2],
    context: lines.slice(0, history).join("\n"),
    before: excerpt.slice(0, -1).join("\n"),
  }
}
