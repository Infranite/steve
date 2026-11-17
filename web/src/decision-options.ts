/* ── decisions whose options are ids ──
 * Most decisions carry their options as words (an agent's question, a flow's branch). A module's decision about a channel
 * message carries the Hub's ids instead ("draft_reply", "to_item", "done"…): the answer and the API use them as they are,
 * and the card shows each one's label, `core:option.<subject>.<id>`. test/decision-options.test.mjs checks every id the
 * Hub defines for these subjects against this list and both languages, so a new option can't show up as its raw id. */
export const OPTION_IDS = {
  chan_message: ["draft_reply", "to_item", "done", "yes", "no", "rewrite", "retry", "discard", "sent"],
} as const satisfies Record<string, readonly string[]>

/** the subject's options are ids with labels of their own */
export const hasOptionIds = (subject: string) => Object.hasOwn(OPTION_IDS, subject)

/** the i18n key of the option's label; undefined when its subject's options are words */
export function optionKey(subject: string, option: string): string | undefined {
  const ids: readonly string[] | undefined = (OPTION_IDS as Record<string, readonly string[]>)[subject]
  return ids?.includes(option) ? `core:option.${subject}.${option}` : undefined
}

/** the option answered with your own words instead of a button: a handoff's reply you write becomes its draft */
export const WORDS_OPTION: Record<string, string> = { chan_message: "draft_reply" }
