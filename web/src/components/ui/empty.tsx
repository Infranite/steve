import { EmptyState, type EmptyStateProps } from "@/components/affect/empty-state-clean"

/* Steve's empty state, drawn by affectUI's Empty State (components/affect/empty-state-clean.tsx): an icon on a soft
 * tile, a title, one line and the first step, framed in a quiet muted fill or bare on a page. It replaces the
 * composable shadcn Empty (EmptyHeader, EmptyMedia, EmptyTitle…), whose one caller, ui.tsx Empty, now passes props.
 * No icon means no tile (affectUI would draw a file). data-slot="empty" stays for tests. */
function Empty({ icon = null, ...props }: EmptyStateProps) {
  return <EmptyState data-slot="empty" icon={icon} {...props} />
}

export { Empty, type EmptyStateProps as EmptyProps }
