import { Separator as AffectSeparator, type SeparatorProps } from "@/components/affect/separator-clean"

/* Steve's Separator, drawn by affectUI's Separator (components/affect/separator-clean.tsx): a hairline in the border
 * color, across or upright, optionally with a word in its middle (label). Same export and orientation prop as the
 * Base UI separator it replaces. It is decorative unless decorative={false}: between the rows of a list (role=list)
 * a role=separator would be a stray child. */
function Separator({ orientation = "horizontal", ...props }: SeparatorProps) {
  return <AffectSeparator data-slot="separator" orientation={orientation} {...props} />
}

export { Separator }
