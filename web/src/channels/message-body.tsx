import { lazy, Suspense } from "react"
import { ChartNoAxesColumnIcon, ExternalLinkIcon, ImageIcon, SquarePenIcon } from "lucide-react"
import { Disclosure } from "@/integrations/kit"
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
  return href ? <a href={href} target="_blank" rel="noopener noreferrer" referrerPolicy="no-referrer" className="inline-flex max-w-full items-center gap-1 break-words rounded-lg bg-blue/8 px-2.5 py-1.5 type-footnote font-medium text-blue hover:bg-blue/12">{children}<ExternalLinkIcon className="size-3 shrink-0" /></a> : <span>{children}</span>
}
function Blocks({ blocks, depth = 0 }: { blocks: CardBlock[]; depth?: number }) {
  const t = useT()
  if (depth > 12) return <p className="text-muted-foreground">{t("channels:card.tooDeep")}</p>
  return <div className="flex min-w-0 flex-col gap-3">
    {blocks.slice(0, 300).map((b, i) => {
      if (!b || typeof b !== "object") return null
      const text = typeof b.text === "string" ? b.text : ""
      const children = Array.isArray(b.blocks) ? b.blocks : []
      switch (b.t) {
        case "markdown": return <Md key={i} text={text} />
        case "heading": return <div key={i} role="heading" aria-level={Math.min(6, Math.max(1, b.level ?? 3))} className="type-headline"><Md text={text} /></div>
        case "hr": return <hr key={i} />
        case "note": return <div key={i} className="type-footnote text-muted-foreground"><Md text={text} /></div>
        case "image": return <div key={i} className="flex items-start gap-2 text-muted-foreground" data-card-image>
          <ImageIcon className="mt-[3px] size-4 shrink-0" /><div className="min-w-0"><div className="break-words text-foreground/80">{b.alt || t("channels:card.imageLabel")}</div><div className="type-footnote">{t("channels:card.imageInFeishu")}</div></div>
        </div>
        case "buttons": return <div key={i} className="flex flex-wrap gap-2" data-card-actions>{(b.items ?? []).slice(0, 50).map((a, n) => {
          const actionable = !a.disabled && !a.callback && cardLink(a.url)
          const why = a.disabled ? t("channels:card.actionDisabled") : t("channels:card.actionInFeishu")
          return actionable ? <Link key={n} url={a.url}>{a.text || t("channels:card.openLink")}</Link> : <div key={n} title={why} className={`max-w-full break-words rounded-lg bg-muted/60 px-2.5 py-1.5 type-footnote font-medium text-muted-foreground ${a.disabled ? "opacity-60" : ""}`} aria-disabled="true">
            {a.text || t("channels:card.action")}<span className="sr-only">{` · ${why}`}</span>
          </div>
        })}{(b.items?.length ?? 0) > 50 && <p className="w-full type-footnote text-muted-foreground">{t("channels:card.moreActions")}</p>}</div>
        /* a column of a short label over its value reads as a field: the label steps down */
        case "columns": return <div key={i} className="grid min-w-0 grid-cols-1 gap-x-4 gap-y-3 sm:grid-flow-col sm:auto-cols-fr [&_.md>:first-child:not(:only-child)]:type-footnote [&_.md>:first-child:not(:only-child)]:text-muted-foreground [&_.md>:first-child:not(:only-child)+*]:mt-0.5!" data-card-columns>
          {(b.columns ?? []).slice(0, 8).map((col, n) => <div key={n} className="min-w-0"><Blocks blocks={Array.isArray(col) ? col : []} depth={depth + 1} /></div>)}{(b.columns?.length ?? 0) > 8 && <p className="col-span-full type-footnote text-muted-foreground">{t("channels:card.moreColumns")}</p>}</div>
        case "table": return <div key={i} className="max-w-full overflow-x-auto rounded-lg bg-muted/60" tabIndex={0} role="region" aria-label={t("channels:card.table")} data-card-table>
          <table className="w-full text-left type-footnote"><thead><tr>{(b.columns ?? []).slice(0, 20).map((col, n) => <th key={n} className="border-b px-3 py-2 type-label text-muted-foreground">{typeof col === "string" ? col : ""}</th>)}</tr></thead>
            <tbody>{(b.rows ?? []).slice(0, 100).map((row, n) => <tr key={n} className="[&:last-child>td]:border-b-0">{row.slice(0, 20).map((cell, j) => <td key={j} className="min-w-20 border-b px-3 py-2 align-top"><Md text={String(cell)} /></td>)}</tr>)}</tbody></table>
        </div>
        /* a control asks for input there: the one muted placeholder */
        case "control": return <div key={i} className="flex flex-col gap-0.5 rounded-lg bg-muted/60 px-3 py-2" data-card-control><div className="text-muted-foreground"><Md text={text} /></div><span className="type-footnote text-tertiary">{t("channels:card.doInFeishu")}</span></div>
        case "panel": return <Disclosure key={i} defaultOpen={!!b.expanded} label={<span className="break-words">{b.title || t("channels:card.moreContent")}</span>} triggerClassName="type-body font-medium text-foreground hover:text-foreground" className="min-w-0" data-card-panel>
          <div className="pl-[18px] pt-2"><Blocks blocks={children} depth={depth + 1} /></div>
        </Disclosure>
        case "group": return <div key={i} className="flex min-w-0 flex-col gap-2"><Blocks blocks={children} depth={depth + 1} />{b.url && <div><Link url={b.url}>{t("channels:card.openLink")}</Link></div>}{b.kind === "form" && <p className="flex items-center gap-1.5 type-footnote text-muted-foreground"><SquarePenIcon className="size-3.5 shrink-0" />{t("channels:card.formInFeishu")}</p>}</div>
        case "chart": return <div key={i} className="flex items-start gap-2"><ChartNoAxesColumnIcon className="mt-[3px] size-4 shrink-0 text-muted-foreground" /><div className="min-w-0"><div className="font-medium">{b.title || t("channels:card.chart")}</div><Md text={b.summary} /><p className="type-footnote text-muted-foreground">{t("channels:card.chartInFeishu")}</p></div></div>
        case "media": return <p key={i} className="text-muted-foreground">{b.kind === "audio" ? t("channels:card.audioInFeishu") : t("channels:card.videoInFeishu")}</p>
        default: return <div key={i} className="text-muted-foreground" data-card-unknown><Md text={text} /><p className="type-footnote">{t("channels:card.unsupported")}</p></div>
      }
    })}
  </div>
}

/* the card's template colour, mapped onto the system's few: the accent or a status, never the provider's palette */
const HEADER_TONE: Record<string, string> = { blue: "before:bg-primary", green: "before:bg-success", red: "before:bg-destructive", orange: "before:bg-attention", yellow: "before:bg-attention", purple: "before:bg-tertiary", grey: "before:bg-tertiary" }
function Card({ card, text }: { card: CardAttachment; text: string | null }) {
  const t = useT()
  const tone = HEADER_TONE[card.template ?? ""]
  return <section aria-label={t("channels:card.label")} className={`relative flex min-w-0 flex-col gap-3 overflow-hidden rounded-xl bg-card p-4 shadow-[var(--lift)] ${tone ? `before:absolute before:inset-x-0 before:top-0 before:h-[3px] ${tone}` : ""}`} data-message-card={card.schema ?? "flattened"}>
    {(card.title || card.subtitle || card.tags?.length) && <header className="flex min-w-0 flex-col gap-1">
      {card.title && <div className="break-words type-headline">{card.title}</div>}
      {card.subtitle && <div className="break-words type-footnote text-muted-foreground">{card.subtitle}</div>}
      {!!card.tags?.length && <div className="flex flex-wrap gap-1">{card.tags.slice(0, 20).map((tag, i) => <span key={i} className="rounded-md bg-muted px-1.5 type-footnote text-muted-foreground">{tag}</span>)}</div>}
    </header>}
    <div className="min-w-0 type-body">
      {card.blocks?.length ? <Blocks blocks={card.blocks} /> : <Md text={text || t("channels:card.noContent")} />}
      {card.truncated && <p className="mt-3 type-footnote text-muted-foreground" data-card-truncated>{t("channels:card.truncated")}</p>}
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
