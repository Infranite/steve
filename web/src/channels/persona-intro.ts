/* the answer to “你是谁？”, kept out of persona.tsx so node --test can load it.
 * Imports use explicit .ts extensions so node can load the files without a bundler. */
import { t } from "../i18n/index.ts"
import type { Persona, Reveal } from "../api/channels.ts"

const NOTHING = "（不透露身份，只回答问题本身）"
/** the answer to “你是谁？” — the Hub's `intro_of` (and its NOTHING_REVEALED), ported so the preview follows every keystroke.
 *  It is what the persona says in the chat, in the Hub's words, so it is not translated. */
export function introOf(p: { name: string; intro: string; reveal: Reveal }, owner: string) {
  const r = p.reveal, bits: string[] = []
  if (r.name) bits.push(`我是${p.name}`)
  if (r.role) bits.push(p.intro.replace(/^我是[^，,。]*[，,]?/, "").replace(/。$/, ""))
  if (r.ai) bits.push("我是 AI 助手")
  if (r.steve) bits.push("由 Steve 驱动")
  if (r.owner) bits.push(`归 ${owner} 管理`)
  const xs = bits.filter(Boolean)
  return bits.length ? xs.join("，") + "。" : NOTHING
}

/** how the Hub names the owner (`<主人称呼>`), read back from its own preview */
export const ownerOf = (p: Persona) => /归 (.+?) 管理/.exec(p.intro_preview)?.[1] ?? "你"

/** what a new persona starts with. The name and intro are stored and said in chats through the Hub's `intro_of`,
 *  which speaks zh-CN ("我是{name}…") and strips a leading "我是…，" from the intro, so they stay in the Hub's language
 *  whatever the UI language is; the user renames it right after. */
export const newPersonaDefaults = () => ({
  name: t("channels:persona.newName", { lng: "zh-CN" }),
  intro: t("channels:persona.newIntro", { lng: "zh-CN" }),
  reveal: { name: true, role: true, ai: true, steve: false, owner: false } satisfies Reveal,
})
