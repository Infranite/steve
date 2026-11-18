/* Message previews inside Steve's affectUI Decision Card: the incoming words and the
 * unsent reply have separate labels and surfaces. Render their words as inert, exact text. */
import { CornerDownRightIcon, MessageSquareIcon } from "lucide-react"
import { useT } from "@/i18n"
import { handoffContent } from "./handoff-content"

export function OriginalMessage({ sender, text, excerpt = false }: { sender: string; text: string | null; excerpt?: boolean }) {
  const t = useT()
  return <section className="min-w-0 rounded-xl border border-border/70 bg-muted/40 p-3" aria-label={t("core:card.originalMessage")} data-card-source>
    <div className="mb-2 flex flex-wrap items-center gap-x-2 gap-y-1">
      <span className="inline-flex items-center gap-1.5 type-footnote text-muted-foreground"><MessageSquareIcon className="size-3.5 shrink-0" aria-hidden />{t(excerpt ? "core:card.originalExcerpt" : "core:card.originalMessage")}</span>
      <strong className="type-callout font-semibold text-foreground [overflow-wrap:anywhere]" data-reply-sender>{sender}</strong>
    </div>
    <blockquote className="m-0! whitespace-pre-wrap text-foreground [overflow-wrap:anywhere]" data-reply-text>{text ?? t("channels:out.cleaned")}</blockquote>
  </section>
}

export function ReplyDraft({ text }: { text: string }) {
  const t = useT()
  return <section className="min-w-0 rounded-xl border border-blue/20 border-l-[3px] border-l-blue bg-blue/5 p-3" aria-label={t("core:card.candidateDraft")} data-card-draft>
    <div className="mb-2 flex flex-wrap items-center justify-between gap-x-3 gap-y-1">
      <span className="inline-flex items-center gap-1.5 font-semibold text-blue"><CornerDownRightIcon className="size-4 shrink-0" aria-hidden />{t("core:card.candidateDraft")}</span>
      <span className="type-footnote text-muted-foreground">{t("core:card.draftNotSent")}</span>
    </div>
    <div className="whitespace-pre-wrap text-foreground [overflow-wrap:anywhere]" data-reply-text>{text}</div>
  </section>
}

export function HandoffPreview({ question, draft }: { question: string; draft?: string }) {
  const t = useT(), content = handoffContent(question)
  const rest = question.split("\n").slice(1).join("\n")
  return <div className="flex min-w-0 flex-col gap-3" data-handoff-preview>
    {content?.space && <div className="type-footnote text-muted-foreground [overflow-wrap:anywhere]" data-reply-space>{content.starred && <span>{t("core:card.starredContact")} · </span>}{content.space}</div>}
    {content && <OriginalMessage sender={content.sender} text={content.text} excerpt />}
    {draft ? <ReplyDraft text={draft} /> : <p className="m-0! type-footnote text-muted-foreground">{t("core:card.noReplyDraft")}</p>}
    {(content?.context || (!content && rest)) && <div className="whitespace-pre-wrap type-footnote text-muted-foreground [overflow-wrap:anywhere]" data-card-context>{content ? content.context : rest}</div>}
    {content?.before && <details className="min-w-0 type-footnote text-muted-foreground" data-reply-history>
      <summary className="cursor-pointer hover:text-foreground">{t("core:card.replyHistory")}</summary>
      <div className="mt-2 whitespace-pre-wrap border-l-2 border-border pl-3 [overflow-wrap:anywhere]">{content.before}</div>
    </details>}
  </div>
}
