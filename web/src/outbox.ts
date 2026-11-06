/** Settings · 数据 · 后台投递: the Hub's `/api/system/outbox` (what the outbox still owes, its dead deliveries by
 * consumer) and what the page says about it. */
/* a relative .ts import so node --test loads this file without a bundler */
import { t } from "./i18n/index.ts"

export type DeadGroup = { consumer: string; topic: string; count: number; last_error: string | null; updated: number }
export type OutboxReport = { pending: number; dead: number; oldest: number | null; groups: DeadGroup[] }

/** One dead group's line: how many, and the last error (cut, it can be a whole response body). */
export function deadSub(g: DeadGroup): string {
  const why = (g.last_error ?? "").replace(/\s+/g, " ").trim()
  const short = why.length > 160 ? why.slice(0, 160) + "…" : why
  return short ? t("shell:outbox.deadSubWhy", { count: g.count, why: short }) : t("shell:outbox.deadSub", { count: g.count })
}

/** The toast after "重新投递". */
export function reviveText(n: number): string {
  return n > 0 ? t("shell:outbox.revived", { count: n }) : t("shell:outbox.nothingRevived")
}
