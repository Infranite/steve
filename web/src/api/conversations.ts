import type { ChanMessage } from "@/api/channels"
/* IM conversations (channels-personas.md §2.12): every space where Steve answers has one conversation,
 * `messages.space = "chan:<space>"`, routed like the web's. The optional list tolerates a Hub without this module;
 * opening a specific conversation keeps request failures distinct so the page can offer a retry. */
import { modReq, isNotWired, type RMessage } from "@/api"

/** where a conversation lives: provider, space, group or DM */
export type ConvBadge = { space: string; space_id: string; name: string; kind: string; dm: boolean; system: string; provider: string; has_external: boolean; member_count: number; mode: "bot" | "self" }
export type ConvRow = ConvBadge & { count: number; last: number; last_text: string; last_author: "me" | "steve"; closed: boolean }
/** the “收到 → 完成” state of an inbound turn's chat message (§2.11) */
export type Reaction = { state: "ack" | "done" | "failed" | "cleared"; emoji: string | null; as_user: boolean; error: string | null; updated: number }
export type ConvMessage = RMessage & { reaction?: Reaction; channel_message?: ChanMessage | null }

const e = encodeURIComponent
const optional = <T>(p: Promise<T>) => p.catch((x) => (isNotWired(x) ? undefined : Promise.reject(x)))

export type ConvPage = { conversations: ConvRow[]; total?: number; has_more?: boolean; next_offset?: number | null }
export const conversationsApi = {
  page: (q: string, state: "open" | "closed" | "all", offset = 0) => modReq<ConvPage>("GET", `/conversations?q=${e(q)}&state=${state}&offset=${offset}&limit=50`),
  preference: (space: string, closed: boolean) => modReq("PATCH", `/conversations/${e(space)}/preferences`, { closed }),
  list: () => optional(modReq<{ conversations: ConvRow[] }>("GET", "/conversations")).then((r) => r?.conversations),
  messages: (space: string) => modReq<{ conversation: ConvBadge; messages: ConvMessage[] }>("GET", `/conversations/${e(space)}/messages`),
}
