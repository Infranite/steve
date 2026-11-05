import { lazy, Suspense } from "react"
import { ExternalLinkIcon, ImageIcon } from "lucide-react"
import { cardLink } from "@/channels/card-link"
import { MessageResource } from "@/channels/message-resource"
import type { ChanMessage } from "@/api/channels"
import { useT } from "@/i18n"

const CardMarkdown = lazy(() => import("@/channels/card-markdown"))
import type { CardAttachment, CardBlock } from "@/api/channel-content"

function Md({ text }: { text?: string }) {
  return text ? <Suspense fallback={<div className="whitespace-pre-wrap break-words">{text}</div>}><CardMarkdown text={text.slice(0, 8000)} /></Suspense> : null
}
function Link({ url, children }: { url?: string; children: React.ReactNode }) {
  const href = cardLink(url)
  return href ? <a href={href} target="_blank" rel="noopener noreferrer" referrerPolicy="no-referrer" className="inline-flex max-w-full items-center gap-1 break-words rounded border px-2.5 py-1.5 type-footnote text-blue hover:bg-muted">{children}<ExternalLinkIcon className="size-3 shrink-0" /></a> : <span>{children}</span>
}
function Blocks({ blocks, depth = 0 }: { blocks: CardBlock[]; depth?: number }) {
  const t = useT()
  if (depth > 12) return <p className="text-muted-foreground">{t("channels:card.tooDeep")}</p>
  return <div className="flex min-w-0 flex-col gap-2">
    {blocks.slice(0, 300).map((b, i) => {
      if (!b || typeof b !== "object") return null
      const text = typeof b.text === "string" ? b.text : ""
      const children = Array.isArray(b.blocks) ? b.blocks : []
      switch (b.t) {
        case "markdown": return <Md key={i} text={text} />
        case "heading": return <div key={i} role="heading" aria-level={Math.min(6, Math.max(1, b.level ?? 3))} className="font-semibold"><Md text={text} /></div>
        case "hr": return <hr key={i} />
        case "note": return <div key={i} className="type-footnote text-muted-foreground"><Md text={text} /></div>
        case "image": return <div key={i} className="flex items-start gap-2 rounded border border-dashed p-2 text-muted-foreground" data-card-image>
          <ImageIcon className="mt-0.5 size-4 shrink-0" /><div><div className="break-words">{b.alt || t("channels:card.imageLabel")}</div><div className="type-footnote">{t("channels:card.imageInFeishu")}</div></div>
        </div>
        case "buttons": return <div key={i} className="flex flex-wrap gap-2" data-card-actions>{(b.items ?? []).slice(0, 50).map((a, n) => {
          const actionable = !a.disabled && !a.callback && cardLink(a.url)
          return actionable ? <Link key={n} url={a.url}>{a.text || t("channels:card.openLink")}</Link> : <div key={n} className="max-w-full rounded border bg-muted/40 px-2.5 py-1.5 type-footnote" aria-disabled="true">
            <span className="break-words">{a.text || t("channels:card.action")}</span><span className="ml-1 text-muted-foreground">{a.disabled ? t("channels:card.actionDisabled") : t("channels:card.actionInFeishu")}</span>
          </div>
        })}{(b.items?.length ?? 0) > 50 && <p className="type-footnote text-muted-foreground">{t("channels:card.moreActions")}</p>}</div>
        case "columns": return <div key={i} className="grid min-w-0 grid-cols-1 gap-3 sm:grid-flow-col sm:auto-cols-fr" data-card-columns>
          {(b.columns ?? []).slice(0, 8).map((col, n) => <div key={n} className="min-w-0"><Blocks blocks={Array.isArray(col) ? col : []} depth={depth + 1} /></div>)}{(b.columns?.length ?? 0) > 8 && <p className="type-footnote text-muted-foreground">{t("channels:card.moreColumns")}</p>}</div>
        case "table": return <div key={i} className="max-w-full overflow-x-auto rounded border" tabIndex={0} role="region" aria-label={t("channels:card.table")} data-card-table>
          <table className="w-full text-left type-footnote"><thead><tr>{(b.columns ?? []).slice(0, 20).map((col, n) => <th key={n} className="border-b bg-muted px-2 py-1.5 font-medium">{typeof col === "string" ? col : ""}</th>)}</tr></thead>
            <tbody>{(b.rows ?? []).slice(0, 100).map((row, n) => <tr key={n}>{row.slice(0, 20).map((cell, j) => <td key={j} className="min-w-20 border-b px-2 py-1.5 align-top last:border-b-0"><Md text={String(cell)} /></td>)}</tr>)}</tbody></table>
        </div>
        case "control": return <div key={i} className="rounded border bg-muted/30 px-2.5 py-2" data-card-control><Md text={text} /><span className="type-footnote text-muted-foreground">{t("channels:card.doInFeishu")}</span></div>
        case "panel": return <details key={i} open={b.expanded} className="min-w-0 rounded border p-2" data-card-panel><summary className="cursor-pointer break-words font-medium">{b.title || t("channels:card.moreContent")}</summary><div className="mt-2"><Blocks blocks={children} depth={depth + 1} /></div></details>
        case "group": return <div key={i} className="min-w-0 rounded border p-2"><Blocks blocks={children} depth={depth + 1} />{b.url && <div className="mt-2"><Link url={b.url}>{t("channels:card.openLink")}</Link></div>}{b.kind === "form" && <p className="mt-1 type-footnote text-muted-foreground">{t("channels:card.formInFeishu")}</p>}</div>
        case "chart": return <div key={i} className="rounded border p-2"><div className="font-medium">{b.title || t("channels:card.chart")}</div><Md text={b.summary} /><p className="type-footnote text-muted-foreground">{t("channels:card.chartInFeishu")}</p></div>
        case "media": return <p key={i} className="text-muted-foreground">{b.kind === "audio" ? t("channels:card.audioInFeishu") : t("channels:card.videoInFeishu")}</p>
        default: return <div key={i} className="text-muted-foreground" data-card-unknown><Md text={text} /><p className="type-footnote">{t("channels:card.unsupported")}</p></div>
      }
    })}
  </div>
}

const HEADER_TONE: Record<string, string> = { blue: "border-t-blue-500", green: "border-t-green-500", red: "border-t-red-500", orange: "border-t-orange-500", purple: "border-t-purple-500", grey: "border-t-gray-400", yellow: "border-t-yellow-500" }
function Card({ card, text }: { card: CardAttachment; text: string | null }) {
  const t = useT()
  return <section aria-label={t("channels:card.label")} className={`min-w-0 overflow-hidden rounded-xl border border-t-4 bg-background ${HEADER_TONE[card.template ?? ""] ?? "border-t-border"}`} data-message-card={card.schema ?? "flattened"}>
    {(card.title || card.subtitle || card.tags?.length) && <header className="border-b bg-muted/30 px-3 py-2.5">
      {card.title && <div className="break-words font-semibold">{card.title}</div>}
      {card.subtitle && <div className="mt-1 break-words type-footnote text-muted-foreground">{card.subtitle}</div>}
      {!!card.tags?.length && <div className="mt-1 flex flex-wrap gap-1">{card.tags.slice(0, 20).map((tag, i) => <span key={i} className="rounded border px-1.5 type-footnote">{tag}</span>)}</div>}
    </header>}
    <div className="min-w-0 px-3 py-3 type-body">
      {card.blocks?.length ? <Blocks blocks={card.blocks} /> : <Md text={text || t("channels:card.noContent")} />}
      {card.truncated && <p className="mt-2 type-footnote text-muted-foreground" data-card-truncated>{t("channels:card.truncated")}</p>}
    </div>
  </section>
}

/* the Hub's `unavailable` codes → message keys, resolved at render time */
const UNAVAILABLE = {
  purged: "channels:unavailable.purged", folder: "channels:unavailable.folder", sticker: "channels:unavailable.sticker", no_key: "channels:unavailable.noKey",
  disconnected: "channels:unavailable.disconnected", unsupported: "channels:unavailable.unsupported",
} as const
const unavailableKey = (code?: string) => (code && Object.hasOwn(UNAVAILABLE, code) ? UNAVAILABLE[code as keyof typeof UNAVAILABLE] : undefined)
/** Shared by the space's recent messages and the read-only IM conversation. */
export function ChannelMessageBody({ message: m }: { message: Pick<ChanMessage, "id" | "text" | "status" | "purged" | "attachments"> }) {
  const t = useT()
  if (m.purged || m.text === null) return <p className="type-body text-muted-foreground" data-message-purged>{t("channels:body.purged")}</p>
  if (m.status === "recalled") return <p className="type-body text-muted-foreground">{t("channels:body.recalled")}</p>
  const attachments = Array.isArray(m.attachments) ? m.attachments.filter(a => a && typeof a === "object") : []
  const card = attachments.find((a): a is CardAttachment => a.type === "card")
  return <div className="flex min-w-0 max-w-full flex-col gap-2" data-channel-body>
    {card ? <Card card={card} text={m.text} /> : m.text ? <div className="whitespace-pre-wrap break-words type-body">{m.text}</div> : !attachments.length && <p className="type-body text-muted-foreground">{m.status === "draft" ? t("channels:body.draftEmpty") : t("channels:body.noText")}</p>}
    {attachments.map((a, i) => {
      if (a.type !== "image" && a.type !== "file") return null
      const expected = `/api/chan-messages/${encodeURIComponent(m.id)}/resources/${i}`
      const url = a.url === expected ? expected : undefined
      const name = a.name || t(a.type === "image" ? "channels:body.image" : a.kind === "audio" ? "channels:body.audio" : a.kind === "video" ? "channels:body.video" : "channels:body.file")
      const why = unavailableKey(a.unavailable)
      return <MessageResource key={`${m.id}:${i}`} url={url} name={name} image={a.type === "image"} unavailable={why ? t(why) : undefined} />
    })}
  </div>
}
