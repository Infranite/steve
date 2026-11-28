/* Unified actor identity model: every "who did it" or person/bot/system reference resolves through
 * one source of truth (member -> linked contact party -> display name + avatar + stable color seed). */
import { fileUrl } from "@/api"
import type { AvatarRef, MemberIdentity } from "@/gen"

export type ActorKind = "person" | "bot" | "system" | "org" | "team"

export type Actor = {
  kind: ActorKind
  /** Canonical identity id: party_id for people/orgs/teams, user_id if unlinked, bot_id for bots, "system" for Hub. */
  id?: string | null
  /** Display name (from person contact, bot name, org/team name, or system label). Never a raw login when a person exists. */
  name: string
  /** Optional distinguishing context (title/org, email, or login) used when display names collide in a list. */
  subtitle?: string | null
  /** Person's picture (`file` stored by Hub or external provider `url`). */
  avatar?: AvatarRef | unknown
  /** Stable color seed: defaults to canonical id (party_id / user_id) so same-named people get distinct colors. */
  seed?: string | null
  /** Bot id when kind === "bot" (Steve renders SteveMascot; other bots render agent-robot-avatar). */
  botId?: string | null
  /** Unconfirmed contact styling (dashed muted placeholder). */
  unconfirmed?: boolean
  /** True when this actor is the signed-in user. */
  isMe?: boolean
}

export type ActorResolveContext = {
  me?: MemberIdentity | null
  members?: readonly MemberIdentity[]
  bots?: Record<string, { n: string; main?: boolean; role?: string }>
  names?: Record<string, string>
  fallbackName?: string
  systemName?: string
}

const SAFE_URL = /^(https?:\/\/|\/|data:image\/)/i

/** Picture URL for an avatar ref: Hub-uploaded file takes precedence over a provider URL. */
export function avatarSrc(a: unknown): string | undefined {
  if (!a || typeof a !== "object") return undefined
  const ref = a as { file?: unknown; url?: unknown }
  if (typeof ref.file === "string" && ref.file.trim()) return fileUrl(ref.file.trim())
  if (typeof ref.url === "string" && SAFE_URL.test(ref.url.trim())) return ref.url.trim()
  return undefined
}

/** True only when the contact has a Hub-uploaded picture (`avatar.file`). */
export function hasUploadedAvatar(a: unknown): boolean {
  if (!a || typeof a !== "object") return false
  const f = (a as { file?: unknown }).file
  return typeof f === "string" && f.trim().length > 0
}

/** Stable palette seed: prefers explicit seed, then canonical person/actor id, then display name. */
export function actorSeed(a: Pick<Actor, "seed" | "id" | "name">): string {
  const s = (a.seed ?? a.id ?? a.name ?? "").trim()
  return s || "?"
}

/** Build an Actor from a Hub MemberIdentity (display name & avatar come from the linked person). */
export function memberActor(m: MemberIdentity, isMe = false): Actor {
  const canonicalId = m.party_id || m.user_id
  const subtitle = m.subtitle || m.email || (m.login && m.login !== m.name ? m.login : undefined)
  return {
    kind: "person",
    id: canonicalId,
    seed: m.avatar?.seed || canonicalId || m.name,
    name: m.name || m.login || m.user_id,
    subtitle,
    avatar: m.avatar,
    isMe,
  }
}

/** Build an Actor from a contact/org/team party record. */
export function partyActor(p: {
  id?: string | null
  name: string
  type?: "contact" | "org" | "team"
  contactKind?: string | null
  avatar?: unknown
  subtitle?: string | null
  unconfirmed?: boolean
  isMe?: boolean
}): Actor {
  const kind: ActorKind =
    p.type === "org" ? "org"
    : p.type === "team" ? "team"
    : p.contactKind === "bot" ? "bot"
    : "person"
  const seed = (p.avatar && typeof p.avatar === "object" && typeof (p.avatar as { seed?: unknown }).seed === "string"
    ? (p.avatar as { seed: string }).seed
    : undefined) || p.id || p.name
  return {
    kind,
    id: p.id ?? undefined,
    seed,
    name: p.name,
    subtitle: p.subtitle ?? undefined,
    avatar: kind === "person" ? p.avatar : undefined,
    unconfirmed: p.unconfirmed,
    isMe: p.isMe,
  }
}

/** Resolve any Hub actor identifier (`usr_*`, `"me"`, bot id, `"system"`, `"hub"`) to an Actor. */
export function resolveActor(rawId: string | null | undefined, ctx: ActorResolveContext): Actor {
  const id = (rawId ?? "").trim()
  const systemLabel = ctx.systemName ?? "System"
  if (!id || id === "system" || id === "hub") {
    return { kind: "system", id: "system", name: ctx.fallbackName || systemLabel }
  }
  if (id === "me") {
    if (ctx.me) return memberActor(ctx.me, true)
    return { kind: "person", id: "me", name: ctx.fallbackName || "Me", isMe: true }
  }
  const member = ctx.members?.find((m) => m.user_id === id || (m.party_id != null && m.party_id === id))
  if (member) return memberActor(member, ctx.me?.user_id === member.user_id)
  if (ctx.me && (ctx.me.user_id === id || (ctx.me.party_id != null && ctx.me.party_id === id))) {
    return memberActor(ctx.me, true)
  }
  const bot = ctx.bots?.[id]
  if (bot) {
    return {
      kind: "bot",
      id,
      botId: id,
      name: bot.main ? bot.n : `${bot.n} Bot`,
      subtitle: bot.role || undefined,
    }
  }
  if (id.startsWith("usr_") || id.startsWith("ct_")) {
    return { kind: "person", id, seed: id, name: ctx.names?.[id] || ctx.fallbackName || id }
  }
  if (id === "agent") {
    return { kind: "bot", id: "agent", botId: null, name: "Agent" }
  }
  return { kind: "system", id, name: ctx.names?.[id] || ctx.fallbackName || id }
}

/**
 * When multiple distinct actors in the same list share a display name, compute a concise
 * disambiguator per actor key (`id ?? index`) so every identity remains distinguishable.
 */
export function disambiguateActors(
  actors: readonly { key: string; name: string; subtitle?: string | null; fallbackId?: string | null }[],
): Map<string, string> {
  const byName = new Map<string, { key: string; subtitle?: string | null; fallbackId?: string | null }[]>()
  for (const a of actors) {
    const norm = a.name.trim().toLocaleLowerCase()
    if (!norm) continue
    const list = byName.get(norm) ?? []
    if (!list.some((x) => x.key === a.key)) list.push(a)
    byName.set(norm, list)
  }
  const out = new Map<string, string>()
  for (const group of byName.values()) {
    if (group.length < 2) continue
    const subCounts = new Map<string, number>()
    for (const item of group) {
      const s = item.subtitle?.trim()
      if (s) subCounts.set(s.toLocaleLowerCase(), (subCounts.get(s.toLocaleLowerCase()) ?? 0) + 1)
    }
    for (const item of group) {
      const sub = item.subtitle?.trim()
      const suffix = (item.fallbackId || item.key).slice(-4)
      if (sub && subCounts.get(sub.toLocaleLowerCase()) === 1) {
        out.set(item.key, sub)
      } else if (sub) {
        out.set(item.key, `${sub} · #${suffix}`)
      } else {
        out.set(item.key, `#${suffix}`)
      }
    }
  }
  return out
}

/** Format a display name with an optional disambiguator suffix ("陈明（研发）" / "Alex Chen (Design)"). */
export function formatActorName(name: string, disambiguator?: string | null, format?: (name: string, extra: string) => string): string {
  const extra = disambiguator?.trim()
  if (!extra) return name
  return format ? format(name, extra) : `${name} (${extra})`
}
