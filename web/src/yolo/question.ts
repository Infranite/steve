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

/** A tool-call question with no recorded command still carries it after its lead ("<lead>: <command>", either colon). Split there,
 *  so the row shows the lead on its line and the command in the code block, like the rows that recorded theirs. */
export function splitAsked(question: string): [string, string] | null {
  const m = question.match(/^([^:\uFF1A\n]+)[:\uFF1A]\s*(\S[\s\S]*)$/)
  return m ? [m[1].trim(), m[2]] : null
}
