/** A tool-call permission asks "允许执行：<command>", and the commands it ran are listed under it as well. When the
 *  question carries the first command (the Hub may clip a long one), the row's line is only what comes before it, so
 *  the command shows once, in the block that can open to its full text. Otherwise the question stays whole. */
export function questionLead(question: string, cmds: string[]): string {
  const first = cmds[0]?.trim()
  if (!first) return question
  const at = question.indexOf(first.slice(0, 24))
  if (at < 0) return question
  const lead = question.slice(0, at).replace(/[\s:\uFF1A]+$/, "") /* ASCII or full-width colon */
  return lead || question
}
