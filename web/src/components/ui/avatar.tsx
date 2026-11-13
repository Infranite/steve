import { Children, cloneElement, isValidElement, type CSSProperties } from "react"
import { useT } from "@/i18n"
import { Avatar as AffectAvatar, AvatarGroup as AffectAvatarGroup, type AvatarProps as AffectAvatarProps, type AvatarGroupProps as AffectAvatarGroupProps, type AvatarSize } from "@/components/affect/avatar-clean"

/* Steve's Avatar, drawn by affectUI's Avatar (components/affect/avatar-clean.tsx): a picture, falling back to initials
 * on a soft color of the person's own, a status dot, and a group that overlaps and ends in +N. It replaces the
 * composable shadcn avatar (AvatarImage, AvatarFallback, AvatarBadge, AvatarGroupCount), which had no callers:
 * affectUI takes the picture as src, the fallback from name, the badge as status and the count from max/total.
 * Sizes: Steve's sm, default and lg (24, 32, 40), affectUI's 20…40, or any pixel size, which scales the initials,
 * the corner and the dot from the nearest of affectUI's steps. */
const STEPS: AvatarSize[] = [20, 24, 32, 40]
const NAMED = { sm: 24, default: 32, lg: 40 } as const

export type AvatarProps = Omit<AffectAvatarProps, "size"> & { size?: keyof typeof NAMED | number }

function scale(size: AvatarProps["size"]): { step: AvatarSize; style?: CSSProperties } {
  const px = typeof size === "number" && Number.isFinite(size) && size > 0 ? size : NAMED[(size as keyof typeof NAMED) ?? "default"] ?? 32
  if (STEPS.includes(px as AvatarSize)) return { step: px as AvatarSize }
  const step = STEPS.reduce((a, b) => (Math.abs(b - px) < Math.abs(a - px) ? b : a))
  return { step, style: { "--s": `${px}px`, "--fs": `${Math.round(px * 0.38 * 2) / 2}px`, "--dot": `${Math.max(6, Math.round(px / 4))}px`, "--sq": `${Math.max(3, Math.round(px / 8))}px`, "--gap": `${Math.max(1, px / 16)}px` } as CSSProperties }
}

function Avatar({ size, style, ...props }: AvatarProps) {
  const s = scale(size)
  return <AffectAvatar data-slot="avatar" size={s.step} style={s.style ? { ...s.style, ...style } : style} {...props} />
}

/* affectUI's group sizes only its own Avatar; Steve's children are this wrapper, so the group hands them its size
 * itself, and names the +N pill in the reader's language. */
function AvatarGroup({ size, children, ...props }: Omit<AffectAvatarGroupProps, "size" | "moreLabel"> & { size?: keyof typeof NAMED | AvatarSize }) {
  const t = useT()
  const px = typeof size === "number" ? size : NAMED[size ?? "default"]
  const sized = Children.map(children, (c) => (isValidElement<AvatarProps>(c) && c.type === Avatar ? cloneElement(c, { size: px }) : c))
  return <AffectAvatarGroup data-slot="avatar-group" size={px} moreLabel={(count) => t("common:a11y.more", { count })} {...props}>{sized}</AffectAvatarGroup>
}

export { Avatar, AvatarGroup }
