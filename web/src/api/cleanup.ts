/* Cleaning up what an identity brought in (接入页 · 断开 / 清理记录): the Rust Hub's /api/channels/cleanup. A dry run
 * first (what would go, what is kept and why), then the purge after an explicit confirmation; the Hub backs up the
 * database before purging and audit-logs it. The TS Hub has none of it: `NotWired` hides the cleanup options. */
import { modReq } from "@/api"

export type CleanupLevel = "text" | "all"
/** a live identity (`connection_id`) or one already disconnected (`account_id`) */
export type CleanupTarget = { connection_id: string } | { account_id: string }
export type CleanupKept = { kind: "space" | "message" | "contact" | string; id: string; name: string; reason: string }
export type CleanupPlan = { level?: CleanupLevel; name: string; spaces: number; messages: number; messages_purged?: number; contacts: number; accounts: number; kept: CleanupKept[]; kept_count: number; backup?: string }
export type Leftover = { account_id: string; name: string; system_id: string; spaces: number; messages: number }

export const cleanupApi = {
  preview: (t: CleanupTarget, level: CleanupLevel) => modReq<CleanupPlan>("POST", "/channels/cleanup/preview", { ...t, level }),
  run: (t: CleanupTarget, level: CleanupLevel) => modReq<CleanupPlan>("POST", "/channels/cleanup", { ...t, level, confirm: true }),
  leftovers: () => modReq<Leftover[]>("GET", "/channels/cleanup/leftovers"),
}

export const LEVEL_TEXT: Record<"keep" | CleanupLevel, [string, string]> = {
  keep: ["保留全部", "群、消息和联系人都留着，以后还能搜到"],
  text: ["删除消息正文", "只删正文和附件，保留谁在什么时候发过消息，事项和决定里的链接不会断"],
  all: ["删除群、消息和自动识别出的联系人", "只删只经过这个身份看到的；被事项、知识或记忆引用的，和你确认或改过的联系人都保留"],
}
