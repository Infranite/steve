/* 一个 Bot 的技能（Rust Hub，steve-modules bot_skills）：一份 SKILL.md——名称、什么时候用（description）和正文。
 * 会话打开时，Node 把启用的技能写到这个 harness 读技能的地方（由 harness 适配器声明，见 RRuntime.skills）。
 * TS Hub 没有这些接口：一律是 NotWired。 */
import { modReq as req } from "@/api"

export type BotSkill = { id: string; bot_id: string; name: string; description: string; content: string; enabled: boolean; created: number; updated: number }
export type BotSkillIn = Partial<{ name: string; description: string; content: string; enabled: boolean }>

const base = (bot: string) => `/bots/${encodeURIComponent(bot)}/skills`
export const botSkillsApi = {
  list: (bot: string) => req<BotSkill[]>("GET", base(bot)),
  create: (bot: string, b: BotSkillIn) => req<BotSkill>("POST", base(bot), b),
  update: (bot: string, id: string, b: BotSkillIn) => req<BotSkill>("PATCH", `${base(bot)}/${encodeURIComponent(id)}`, b),
  remove: (bot: string, id: string) => req<{ ok: true }>("DELETE", `${base(bot)}/${encodeURIComponent(id)}`),
}
