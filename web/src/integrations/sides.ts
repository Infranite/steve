import { hubUrl } from "../hub.ts"
/* The channel sides as the Hub declares them (`GET /connections/integrations` `adapters[].channel` and `pairing`): who acts
 * as you and pairs with which bot, how a connection is made, how messages come in. Pure, so the node tests import it. */

export type StartMethod = { kind: "oauth" } | { kind: "device" } | { kind: "form" } | { kind: "route"; path: string; label: string }
export type Receive = "long_connection" | "webhook" | "poll" | "stream"
export type ChannelSummary = {
  adapter: string; handle_system: string; mode: "bot" | "self"; features: Record<string, boolean>
  receive: Receive; receive_cap: string | null; message_ids: "platform" | "none"; created_precision_ms: number; start: StartMethod[]
}
export type Pairing = { with: string[]; required: boolean }
/** the part of an integrations adapter the sides need */
export type SideAdapter = { id: string; identity: string; auth?: string; device?: boolean; pairing?: Pairing | null; channel?: ChannelSummary | null }

/** the adapters whose connections are a provider's bot */
export const botAdapterIds = (adapters: SideAdapter[]): string[] => adapters.filter((a) => a.identity === "bot").map((a) => a.id)

/** the sides that act as you and pair with a connection of adapter `bot` (an older Hub without `pairing`: every side
 * that acts as you); none means there is nothing to authorize as you through that bot */
export const selfSidesFor = (adapters: SideAdapter[], bot: string): SideAdapter[] =>
  adapters.filter((a) => a.identity === "self" && (a.pairing === undefined || (a.pairing?.with ?? []).includes(bot)))

/** the first side acting as you through any of `bots` */
export const selfSideOf = (adapters: SideAdapter[], bots: string[] = botAdapterIds(adapters)): SideAdapter | null =>
  bots.map((b) => selfSidesFor(adapters, b)[0]).find(Boolean) ?? null

/** how a connection of `a` is made, in the order offered: its declared start; an adapter without a declaration, its
 * OAuth flow (and the device code when offered) or its credential form */
export function startsOf(a: SideAdapter | null | undefined): StartMethod[] {
  if (!a) return [{ kind: "oauth" }, { kind: "device" }]
  if (a.channel?.start) return a.channel.start
  const out: StartMethod[] = []
  if (a.auth === "oauth") { out.push({ kind: "oauth" }); if (a.device) out.push({ kind: "device" }) }
  else if (a.auth === "credential") out.push({ kind: "form" })
  return out
}

export const offers = (starts: StartMethod[], kind: StartMethod["kind"]) => starts.some((s) => s.kind === kind)

/** the extension's own pages among `starts`, as links under the Hub's API */
export const routesOf = (starts: StartMethod[]): { href: string; label: string }[] =>
  starts.flatMap((s) => (s.kind === "route" && s.path.startsWith("/") && !s.path.startsWith("//") ? [{ href: hubUrl("/api" + s.path), label: s.label }] : []))

/** the account page's line about receiving, by the side's declaration */
export const receiveNote = (r: Receive | string | null | undefined): "polling" | "longConnection" | null =>
  r === "poll" ? "polling" : r === "long_connection" ? "longConnection" : null
