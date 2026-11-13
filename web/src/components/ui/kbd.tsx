import type { ComponentProps } from "react"
import { Kbd as AffectKbd, type KbdProps } from "@/components/affect/kbd-clean"
import { useT } from "@/i18n"
import { cn } from "cn"

/* Steve's Kbd, drawn by affectUI's Kbd (components/affect/kbd-clean.tsx): a key chip tinted from the text around it,
 * so it reads in a tooltip, on a button and in either theme. Same export: children are the key as written (⌘B, ⏎);
 * affectUI's keys={["mod", "K"]} (⌘ on a Mac, Ctrl elsewhere, spelled out for screen readers) also passes through.
 * 20px high, as Steve's was; in a tooltip it takes the tooltip's own ink. */
/** "Up arrow" → keyName.upArrow; a letter or a key with no name of its own is read as it is */
const keyId = (spoken: string) => spoken.toLowerCase().replace(/ (\w)/g, (_, c: string) => c.toUpperCase())

function Kbd({ className, size = "md", ...props }: KbdProps) {
  const t = useT()
  const speak = (spoken: string) => (spoken.length > 1 ? t(`common:keyName.${keyId(spoken)}` as "common:keyName.command", { defaultValue: spoken }) : spoken)
  return <AffectKbd data-slot="kbd" size={size} speak={speak} className={cn("pointer-events-none [&_svg:not([class*='size-'])]:size-3", className)} {...props} />
}

function KbdGroup({ className, ...props }: ComponentProps<"kbd">) {
  return <kbd data-slot="kbd-group" className={cn("inline-flex items-center gap-1", className)} {...props} />
}

export { Kbd, KbdGroup }
