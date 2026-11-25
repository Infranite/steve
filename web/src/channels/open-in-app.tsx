/* Provider-aware "Open in <app>" button and menu label (./open-link.ts).
 * Uses `provider` from the Hub's open-link contract; falls back to recognizing Feishu/Lark URLs
 * only when older payloads omit `open_provider`, and otherwise renders a generic source-app label. */
import { ExternalLinkIcon } from "lucide-react"
import { cn } from "cn"
import { Button } from "@/components/ui/button"
import { t as tr, useT } from "@/i18n"
import { openable } from "@/channels/open-link"
import { useQuery } from "@tanstack/react-query"
import { api } from "@/api"
import { qc, scopeKey } from "@/data/sync"

export { openable, openInApp } from "@/channels/open-link"

function isFeishuHost(url?: string | null): boolean {
  if (!url) return false
  try {
    const h = new URL(url).hostname.toLowerCase()
    return h === "feishu.cn" || h.endsWith(".feishu.cn") || h === "larksuite.com" || h.endsWith(".larksuite.com")
  } catch {
    return false
  }
}

/** Human-facing app name for a channel provider id (`feishu` → localized Feishu; other ids pass through). */
export function providerAppName(provider?: string | null, url?: string | null): string {
  const p = provider?.trim()
  if (p) {
    const low = p.toLowerCase()
    if (low === "feishu" || low === "feishu_app" || low === "feishu_self") return tr("channels:word.feishu")
    if (low === "lark") return "Lark"
    return p
  }
  if (isFeishuHost(url)) return tr("channels:word.feishu")
  return ""
}

/** Label for menus and buttons that open the chat or message in its source app. */
export function openInSourceAppLabel(provider?: string | null, url?: string | null): string {
  const p = provider?.trim()
  const low = p?.toLowerCase()
  if (
    low === "feishu"
    || low === "feishu_app"
    || low === "feishu_self"
    || (p && p.toLowerCase() === tr("channels:word.feishu").toLowerCase())
    || (!p && isFeishuHost(url))
  ) {
    return tr("channels:word.openInFeishu")
  }
  if (p) return tr("channels:word.openInProvider", { provider: low === "lark" ? "Lark" : p })
  return tr("channels:word.openInSourceApp")
}

/** Small external-link button; renders nothing without an https open URL. */
export function OpenInSourceApp({ url, provider, className, compact }: {
  url?: string | null
  provider?: string | null
  className?: string
  /** on a phone only the icon (the label stays its accessible name) */
  compact?: boolean
}) {
  useT()
  const href = openable(url)
  if (!href) return null
  const label = openInSourceAppLabel(provider, href)
  return (
    <Button size="xs" variant="ghost" className={cn("shrink-0 text-muted-foreground", className)} nativeButton={false}
      render={<a href={href} target="_blank" rel="noopener noreferrer" />} data-open-in-app>
      <ExternalLinkIcon /><span className={cn(compact && "max-sm:sr-only")}>{label}</span>
    </Button>
  )
}

/** Open-in-source-app button for a chat message referenced by a card: fetched on demand. */
export function MessageOpenInSourceApp({ message, provider, className }: { message?: string; provider?: string | null; className?: string }) {
  const { data } = useQuery({ queryKey: ["detail", scopeKey(), "open-url", message], queryFn: () => api.messageOpenUrl(message!), enabled: !!message, staleTime: 5 * 60_000 }, qc)
  return <OpenInSourceApp url={data?.url} provider={data?.provider ?? provider} className={className} />
}
