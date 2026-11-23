/* “在飞书中打开” as a small link button, and its label for menus that draw their own items (./open-link.ts) */
import { ExternalLinkIcon } from "lucide-react"
import { cn } from "cn"
import { Button } from "@/components/ui/button"
import { t as tr, useT } from "@/i18n"
import { openable } from "@/channels/open-link"
import { useQuery } from "@tanstack/react-query"
import { api } from "@/api"
import { qc, scopeKey } from "@/data/sync"

export { openable, openInApp } from "@/channels/open-link"

/** the label, for menus that draw their own items */
export const openInFeishuLabel = () => tr("channels:word.openInFeishu")

/** a small link button; nothing without a link */
export function OpenInFeishu({ url, className, compact }: { url?: string | null; className?: string; /** on a phone only the icon (the label stays its name) */ compact?: boolean }) {
  const t = useT()
  const href = openable(url)
  if (!href) return null
  return (
    <Button size="xs" variant="ghost" className={cn("shrink-0 text-muted-foreground", className)} nativeButton={false}
      render={<a href={href} target="_blank" rel="noopener noreferrer" />} data-open-in-app>
      <ExternalLinkIcon /><span className={cn(compact && "max-sm:sr-only")}>{t("channels:word.openInFeishu")}</span>
    </Button>
  )
}

/** the button for a chat message a card is about: its link is asked for when the card shows, not with every list */
export function MessageOpenInFeishu({ message, className }: { message?: string; className?: string }) {
  const { data } = useQuery({ queryKey: ["detail", scopeKey(), "open-url", message], queryFn: () => api.messageOpenUrl(message!), enabled: !!message, staleTime: 5 * 60_000 }, qc)
  return <OpenInFeishu url={data?.url} className={className} />
}
