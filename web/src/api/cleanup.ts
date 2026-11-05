/* Cleaning up what an identity brought in (接入页 · 断开 / 清理记录): the Rust Hub's /api/channels/cleanup. A dry run
 * first (what would go, what is kept and why), then the purge after an explicit confirmation; the Hub backs up the
 * database before purging and audit-logs it. The TS Hub has none of it: `NotWired` hides the cleanup options. */
import { modReq } from "@/api"
import { lazy } from "@/api/accounts"
import { t } from "@/i18n"

export type CleanupLevel = "text" | "all"
/** a live identity (`connection_id`) or one already disconnected (`account_id`) */
export type CleanupTarget = { connection_id: string } | { account_id: string }
export type CleanupKept = { kind: "space" | "message" | "contact" | string; id: string; name: string; reason: string }
export type CleanupPlan = { level?: CleanupLevel; name: string; spaces: number; messages: number; messages_purged?: number; contacts: number; accounts: number; decisions?: number; decision_ids?: string[]; previews?: CleanupPlan[]; kept: CleanupKept[]; kept_count: number; backup?: string }
export type Leftover = { account_id: string; name: string; system_id: string; spaces: number; messages: number }

export const cleanupApi = {
  preview: (t: CleanupTarget, level: CleanupLevel) => modReq<CleanupPlan>("POST", "/channels/cleanup/preview", { ...t, level }),
  run: (t: CleanupTarget, level: CleanupLevel, decision_ids: string[] = []) => modReq<CleanupPlan>("POST", "/channels/cleanup", { ...t, level, confirm: true, decision_ids }),
  leftovers: () => modReq<Leftover[]>("GET", "/channels/cleanup/leftovers"),
}

export const LEVEL_TEXT: Record<"keep" | CleanupLevel, [string, string]> = lazy({
  keep: () => [t("api:cleanup.keep"), t("api:cleanup.keepSub")],
  text: () => [t("api:cleanup.text"), t("api:cleanup.textSub")],
  all: () => [t("api:cleanup.all"), t("api:cleanup.allSub")],
})
