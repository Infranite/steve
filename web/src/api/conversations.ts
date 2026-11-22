import type { ChanMessage } from "@/api/channels"
/* IM conversations (channels-personas.md §2.12): every space where Steve answers has one conversation,
 * `messages.space = "chan:<space>"`, routed like the web's. The optional list tolerates a Hub without this module;
 * opening a specific conversation keeps request failures distinct so the page can offer a retry. */
import { modReq, isNotWired, type ConversationMessage } from "@/api"

/** where a conversation lives: provider, space, group or DM */
export type ConvBadge = { space: string; space_id: string; name: string; kind: string; dm: boolean; system: string; provider: string; has_external: boolean; member_count: number; mode: "bot" | "self"
  /** opens the chat in its platform's own client (“在飞书中打开”); null when it has none (newer Hubs) */
  open_url?: string | null
  /** the channel account that speaks there; null once disconnected (newer Hubs) */
  connection?: string | null }
/** `unread`: new messages since you last opened it here, counted up to 100 (newer Hubs) */
export type ConvRow = ConvBadge & { count: number; last: number; last_text: string; last_author: "me" | "steve"; closed: boolean; unread?: number }
/** the “收到 → 完成” state of an inbound turn's chat message (§2.11) */
export type Reaction = { state: "ack" | "done" | "failed" | "cleared"; emoji: string | null; as_user: boolean; error: string | null; updated: number }
export type ConvMessage = ConversationMessage & { reaction?: Reaction; channel_message?: ChanMessage | null }

/** what Steve did with a received message of the record (#243) */
export type HandlingState = "answered" | "drafted" | "blocked" | "unknown" | "recalled" | "local" | "waiting" | "item" | "working" | "failed" | "skipped" | "decided" | "recorded" | "none"
/** `main_turn`: your `/bot` command answered in your own conversation (no account of yours may answer in the chat): its turn there */
export type Handling = { state: HandlingState; label: string; reason: string; error: string | null; item: string | null; decision: string | null; main_turn?: string | null; out_message_id?: string | null; out_status?: string | null }
/** one message of a channel conversation's record: received ones say what Steve did with them, sent ones their send state;
 *  `turn`: the conversation message that already shows it */
export type ConvRecord = ChanMessage & { turn: string | null; handling?: Handling; out?: { status: string; decision: string | null; error: string | null } }
export type RecordPage = { records: ConvRecord[]; has_more: boolean; next: { before: number; before_id: string } | null }
/** an entry of the channel switcher: a channel account you see and how many of its conversations have new messages */
export type ChannelEntry = { connection: string; name: string; provider: string; mode: "bot" | "self"; status: string; conversations: number; unread: number }

const e = encodeURIComponent
const optional = <T>(p: Promise<T>) => p.catch((x) => (isNotWired(x) ? undefined : Promise.reject(x)))

export type ConvPage = { conversations: ConvRow[]; total?: number; has_more?: boolean; next_offset?: number | null }
export type ConvState = "open" | "closed" | "all" | "replied"
export const conversationsApi = {
  page: (q: string, state: ConvState, offset = 0, connection = "") => modReq<ConvPage>("GET", `/conversations?q=${e(q)}&state=${state}&offset=${offset}&limit=50${connection ? `&connection=${e(connection)}` : ""}`),
  /** the channel switcher; undefined on a Hub without it */
  channels: () => optional(modReq<{ channels: ChannelEntry[] }>("GET", "/conversations/channels")).then((r) => r?.channels),
  /** a page of the record, oldest first; undefined on a Hub without it */
  record: (space: string, before?: RecordPage["next"], limit = 100) => optional(modReq<RecordPage>("GET", `/conversations/${e(space)}/record?limit=${limit}${before ? `&before=${before.before}&before_id=${e(before.before_id)}` : ""}`)),
  read: (space: string, at: number) => modReq("POST", `/conversations/${e(space)}/read`, { at }),
  preference: (space: string, closed: boolean) => modReq("PATCH", `/conversations/${e(space)}/preferences`, { closed }),
  list: () => optional(modReq<{ conversations: ConvRow[] }>("GET", "/conversations")).then((r) => r?.conversations),
  messages: (space: string) => modReq<{ conversation: ConvBadge; messages: ConvMessage[] }>("GET", `/conversations/${e(space)}/messages`),
}
