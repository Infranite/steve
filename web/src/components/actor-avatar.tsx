/* Single avatar component across the entire app:
 * - Person -> uploaded picture or initials + stable color (affectUI Avatar)
 * - Steve & Bots -> unified charcoal mascot family (Robot -> SteveMascot)
 * - System -> crisp system badge
 * - Org / Team -> rounded square glyph on stable color (affectUI Avatar)
 * Hovering shows display name and actor type (Person / Bot / System / Org / Team). */
import { BuildingIcon, BotIcon, CpuIcon, UsersIcon } from "lucide-react"
import { cn } from "cn"
import { Avatar } from "@/components/ui/avatar"
import { Tooltip as TooltipPrimitive } from "@/components/affect/tooltip-clean"
import { Robot } from "@/robot"
import { st } from "@/store"
import { useT } from "@/i18n"
import { actorSeed, avatarSrc, formatActorName, resolveActor, type Actor } from "@/actor"

export type ActorAvatarProps = {
  /** Pre-built Actor object (person, bot, system, org, or team). */
  actor?: Actor
  /** Actor id (`usr_*`, `"me"`, bot id, `"system"`), resolved against workspace members & bots. */
  actorId?: string | null
  /** Fallback name when resolving by `actorId`. */
  name?: string
  /** Optional disambiguator shown when duplicate names exist in the same list. */
  disambiguator?: string | null
  /** Pixel size (default 24). */
  size?: number
  /** Animate the bot's face (chat header only). */
  live?: boolean
  /** Render hover tooltip with name + actor type (default true; disable inside buttons that provide their own tooltip). */
  tooltip?: boolean
  className?: string
  /** Optional test marker for the account avatar. */
  account?: boolean
}

export function ActorAvatar({
  actor,
  actorId,
  name,
  disambiguator,
  size = 24,
  live = false,
  tooltip = true,
  className,
  account = false,
}: ActorAvatarProps) {
  const t = useT()
  const resolved: Actor =
    actor ??
    resolveActor(actorId, {
      me: st.me,
      members: st.members,
      bots: st.bots,
      fallbackName: name,
      systemName: t("common:actor.kind.system"),
    })
  const kindLabel = t(`common:actor.kind.${resolved.kind}`)
  const displayName = formatActorName(resolved.name || kindLabel, disambiguator, (n, extra) =>
    t("common:actor.disambiguated", { name: n, extra }),
  )
  const hoverLabel =
    resolved.kind === "system" && displayName.trim().toLocaleLowerCase() === kindLabel.trim().toLocaleLowerCase()
      ? kindLabel
      : t("common:actor.hover", { name: displayName, kind: kindLabel })

  let face: React.ReactNode
  if (resolved.kind === "bot") {
    const botKey = resolved.botId ?? resolved.id
    if (botKey && (st.bots[botKey] || botKey === "steve")) {
      face = <Robot k={botKey} size={size} live={live} />
    } else {
      face = (
        <span
          className="inline-flex shrink-0 select-none items-center justify-center rounded-full bg-muted text-muted-foreground"
          style={{ width: size, height: size }}
          aria-hidden
        >
          <BotIcon style={{ width: size * 0.56, height: size * 0.56 }} strokeWidth={1.75} />
        </span>
      )
    }
  } else if (resolved.kind === "system") {
    face = (
      <span
        className="inline-flex shrink-0 select-none items-center justify-center rounded-full bg-muted text-muted-foreground"
        style={{ width: size, height: size }}
        aria-hidden
      >
        <CpuIcon style={{ width: size * 0.54, height: size * 0.54 }} strokeWidth={1.75} />
      </span>
    )
  } else {
    const isPerson = resolved.kind === "person"
    const icon =
      resolved.kind === "org" ? <BuildingIcon strokeWidth={1.5} />
      : resolved.kind === "team" ? <UsersIcon strokeWidth={1.5} />
      : undefined
    face = (
      <Avatar
        name={resolved.name}
        src={isPerson ? avatarSrc(resolved.avatar) : undefined}
        seed={actorSeed(resolved)}
        size={size}
        shape={isPerson ? "circle" : "square"}
        icon={icon}
        aria-label=""
        className="shrink-0"
        data-unconfirmed={resolved.unconfirmed || undefined}
      />
    )
  }

  const node = (
    <span
      className={cn("inline-flex shrink-0 items-center justify-center align-middle", className)}
      role="img"
      aria-label={hoverLabel}
      data-actor-avatar=""
      data-actor-kind={resolved.kind}
      data-actor-id={resolved.id ?? undefined}
      data-actor-name={resolved.name}
      data-actor-tooltip={hoverLabel}
      data-account-avatar={account ? "" : undefined}
    >
      {face}
    </span>
  )

  if (!tooltip) return node
  return (
    <TooltipPrimitive content={hoverLabel} delay={250} side="top" className="steve-tip">
      {node}
    </TooltipPrimitive>
  )
}
