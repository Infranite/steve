/* Who is signed in: unified person identity from `/api/shell` (`st.me`), falling back to `/api/me`
 * when a page or test loads before shell includes `me`. Used by the sidebar account row and mobile `MineButton`. */
import { useEffect, useState } from "react"
import { cn } from "cn"
import { api } from "@/api"
import { memberActor } from "@/actor"
import { ActorAvatar } from "@/components/actor-avatar"
import type { MemberIdentity } from "@/gen"
import { useT } from "@/i18n"
import { st, useStoreSelector } from "@/store"

type RawMeUser = {
  id?: string
  user_id?: string
  party_id?: string | null
  name?: string
  login?: string
  email?: string | null
  role?: string
  avatar?: MemberIdentity["avatar"]
  subtitle?: string | null
}

function toMember(raw: { person?: MemberIdentity | null; user?: RawMeUser | null }): MemberIdentity | null {
  if (raw.person) return raw.person
  const u = raw.user
  if (!u) return null
  const userId = u.user_id ?? u.id ?? ""
  const name = u.name ?? u.login ?? ""
  const login = u.login ?? u.name ?? ""
  return {
    user_id: userId,
    party_id: u.party_id ?? undefined,
    name,
    login,
    email: u.email ?? undefined,
    role: u.role ?? "",
    avatar: u.avatar,
    subtitle: u.subtitle ?? undefined,
  }
}

let asked: Promise<MemberIdentity | null> | null = null

/** Unified identity of the signed-in user (display name & avatar come from their linked person). */
export function useMe(): MemberIdentity | null {
  const fromShell = useStoreSelector(() => st.me)
  const [fallback, setFallback] = useState<MemberIdentity | null>(null)
  useEffect(() => {
    if (fromShell) return
    let on = true
    asked ??= api.me().then((r) => toMember(r as { person?: MemberIdentity | null; user?: RawMeUser | null }), () => { asked = null; return null })
    void asked.then((m) => { if (on) setFallback(m) })
    return () => { on = false }
  }, [fromShell])
  return fromShell ?? fallback
}

/** a phone's way to 我的: your avatar, with a dot when a newer Steve can be installed. Not on 我的 itself, nor wider
 *  than a phone, where the sidebar's account row does this */
export function MineButton({ className }: { className?: string }) {
  const t = useT(), me = useMe()
  if (location.hash.replace(/^#\/?/, "").split(/[/?]/)[0] === "more") return null
  const actor = me ? memberActor(me, true) : { kind: "person" as const, name: t("core:nav.mine") }
  return (
    <a href="#/more" aria-label={t("core:nav.mine")} className={cn("relative flex size-8 shrink-0 items-center justify-center rounded-full md:hidden", className)} data-mine>
      <ActorAvatar actor={actor} size={28} tooltip={false} account />
      {st.update && <span className="absolute -right-0.5 -top-0.5 size-2 rounded-full bg-attention ring-2 ring-background" data-mine-update-dot />}
    </a>
  )
}
