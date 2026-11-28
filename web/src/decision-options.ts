/* ── decisions whose options are ids ──
 * Most decisions carry their options as words (an agent's question, a flow's branch). A module's decision about a channel
 * message carries the Hub's ids instead ("instruct", "to_item", "done"…): the answer and the API use them as they are,
 * and the card shows each one's label, `core:option.<subject>.<id>`. test/decision-options.test.mjs checks every id the
 * Hub defines for these subjects against this list and both languages, so a new option can't show up as its raw id. */
export const OPTION_IDS = {
  chan_message: ["send_draft", "instruct", "verbatim", "to_item", "done", "yes", "no", "rewrite", "retry", "discard", "sent"],
  wf_run: ["retry", "revise", "cancel", "single_session", "add_budget"],
} as const satisfies Record<string, readonly string[]>

/** the subject's options are ids with labels of their own */
export const hasOptionIds = (subject: string) => Object.hasOwn(OPTION_IDS, subject)

/** the i18n key of the option's label; undefined when its subject's options are words */
export function optionKey(subject: string, option: string): string | undefined {
  const ids: readonly string[] | undefined = (OPTION_IDS as Record<string, readonly string[]>)[subject]
  return ids?.includes(option) ? `core:option.${subject}.${option}` : undefined
}

/** the options answered with your own words instead of a button: on a message handed over to you, how to reply
 * (the field's send button: the composer drafts with your words) */
export const WORDS_OPTIONS: Record<string, readonly string[]> = { chan_message: ["instruct"] }
/** the options that send your typed words along with the click (“按原文发”); their button needs words */
export const WITH_WORDS: Record<string, readonly string[]> = { chan_message: ["verbatim"] }


export const FLOW_SUBJECTS = ["flow_step", "signal", "flow_run"]
export const isPeopleDecision = (w: { type: string; subject: string }) => w.subject === "party_merge" || w.type === "confirm"
const MODULE_SUBJECTS = ["secret_use", "connection", "space"] as const
export type ModuleCardSubject = typeof MODULE_SUBJECTS[number]
export const isModuleCard = (subject: string): subject is ModuleCardSubject => (MODULE_SUBJECTS as readonly string[]).includes(subject)
