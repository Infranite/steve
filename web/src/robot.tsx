/* Unified charcoal mascot faces for Steve and all Bots:
 * - Steve (`k === "steve"` or `st.bots[k]?.main`) renders his monochrome classic circle charcoal mascot with no accessories.
 * - Ordinary Bots render the same charcoal family with color, head roundness, and optional role accessory
 *   (color & roundness stably hashed from Bot id when unset; accessory stays empty unless explicitly set).
 * - Static sizes render synchronously cached SVGs with zero hidden DOM; `live` instances follow the pointer
 *   and react to Steve's cue bus through the single shared demand-driven animation clock. */
import { st, defaultColor, defaultRound } from "@/store"
import { SteveMascot, type BotAccessory } from "@/steve-mascot"

export { AVATAR_COLORS } from "@/store"
export type Look = { color: string; round: number; accessory?: BotAccessory | null }

export const lookOf = (k: string): Look => {
  const b = st.bots[k]
  return b
    ? { color: b.color, round: b.round, accessory: b.accessory ?? null }
    : { color: defaultColor(k), round: defaultRound(k), accessory: null }
}

/** Static preview of a Bot charcoal mascot (used in Bot appearance settings). */
export function Face({ look, size, className, id }: { look: Look; size: number; className?: string; id?: string }) {
  return (
    <SteveMascot
      appearance={{ kind: "bot", id: id ?? look.color, color: look.color, round: look.round, accessory: look.accessory ?? null }}
      size={size}
      className={className}
    />
  )
}

export function Robot({
  k,
  size = 24,
  live,
  className,
  color,
  round,
  accessory,
}: {
  k: string
  size?: number
  live?: boolean
  className?: string
  color?: string
  round?: number
  accessory?: BotAccessory | null
}) {
  if (k === "steve" || st.bots[k]?.main) {
    return <SteveMascot appearance={{ kind: "steve" }} size={size} live={live} className={className} />
  }
  const base = lookOf(k)
  return (
    <SteveMascot
      appearance={{
        kind: "bot",
        id: k,
        color: color ?? base.color,
        round: round ?? base.round,
        accessory: accessory !== undefined ? accessory : (base.accessory ?? null),
      }}
      size={size}
      live={live}
      className={className}
    />
  )
}

/** Message tint for a bot — a whisper of its colour. */
export const tintOf = (k: string) =>
  `color-mix(in srgb, ${lookOf(k).color} ${document.documentElement.classList.contains("dark") ? 14 : 12}%, var(--background))`
