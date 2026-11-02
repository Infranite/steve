/* IM conversations (channels-personas.md §2.12): every space where Steve answers has one conversation,
 * `messages.space = "chan:<space>"`, routed like the web's. The Rust Hub's /api/conversations; the TS Hub has none
 * (`undefined`: the 对话 page then shows only your own conversation). Read-only here. */
import { modReq, isNotWired, type RMessage } from "@/api"

/** where a conversation lives: provider, space, group or DM */
export type ConvBadge = { space: string; space_id: string; name: string; kind: string; dm: boolean; system: string; provider: string; has_external: boolean; member_count: number; mode: "bot" | "self" }
export type ConvRow = ConvBadge & { count: number; last: number; last_text: string; last_author: "me" | "steve" }
/** the “收到 → 完成” state of an inbound turn's chat message (§2.11) */
export type Reaction = { state: "ack" | "done" | "failed" | "cleared"; emoji: string | null; as_user: boolean; error: string | null; updated: number }
export type ConvMessage = RMessage & { reaction?: Reaction }

const e = encodeURIComponent
const optional = <T>(p: Promise<T>) => p.catch((x) => (isNotWired(x) ? undefined : Promise.reject(x)))

export const conversationsApi = {
  list: () => optional(modReq<{ conversations: ConvRow[] }>("GET", "/conversations")).then((r) => r?.conversations),
  messages: (space: string) => optional(modReq<{ conversation: ConvBadge; messages: ConvMessage[] }>("GET", `/conversations/${e(space)}/messages`)),
}
