/* What an outbound message answers, above its draft on the approval card: the message it replies to
 * (with the few before it, folded), where it goes and as whom. Hidden when the Hub has no such route. */
import { useEffect, useState } from "react"
import { outContext, type ROutContext } from "@/api/channels"
import { useT } from "@/i18n"

/* the space kinds this card names; others show the Hub's kind as is */
const KINDS = ["group", "topic_group", "channel", "dm", "self_dm"] as const
type Kind = (typeof KINDS)[number]
const known = (k: string): k is Kind => (KINDS as readonly string[]).includes(k)

export function OutContext({ id }: { id?: string }) {
  const t = useT()
  const [c, setC] = useState<ROutContext | null>(null)
  const [open, setOpen] = useState(false)
  useEffect(() => { if (!id) return; let live = true; outContext(id).then((x) => live && setC(x ?? null), () => live && setC(null)); return () => { live = false } }, [id])
  if (!c) return null
  const who = c.as_you ? t("channels:out.asYou") : c.persona ? t("channels:out.asPersona", { persona: c.persona }) : t("channels:out.asBot")
  const kind = c.space.kind
  return (
    <div className="mb-3 flex flex-col gap-2 rounded-xl bg-muted/60 px-4 py-3 type-callout" data-out-context>
      <div className="flex flex-wrap items-center gap-x-1.5 type-footnote text-muted-foreground">
        <button type="button" className="font-medium text-foreground/80 hover:underline" onClick={() => { location.hash = "#/channels/space/" + c.space.id }}>{c.space.name}</button>
        <span>{known(kind) ? t(`channels:out.kind.${kind}`) : kind}</span>
        {c.space.external && <span className="text-attention">{t("channels:word.externalMembers")}</span>}
        <span className="text-tertiary">·</span><span>{who}</span>
      </div>
      {c.reply_to ? <>
        {c.before.length > 0 && (open
          ? <div className="flex flex-col gap-1 border-l-2 pl-2.5 type-footnote text-muted-foreground anim-in" data-out-before>{c.before.map((b, n) => <div key={n} className="break-words"><span className="font-medium">{t("channels:out.sender", { name: b.sender })}</span>{b.text ?? t("channels:out.cleaned")}</div>)}</div>
          : <button type="button" className="self-start type-footnote text-blue hover:underline" onClick={() => setOpen(true)}>{t("channels:out.before", { count: c.before.length })}</button>)}
        <div className="break-words" data-out-reply-to><span className="font-medium">{t("channels:out.sender", { name: c.reply_to.sender })}</span>{c.reply_to.text ?? t("channels:out.cleaned")}</div>
        <div className="type-footnote text-muted-foreground">{t("channels:out.replyTo", { name: c.reply_to.sender })}</div>
      </> : <div className="type-footnote text-muted-foreground">{t(`channels:out.direct.${known(kind) ? kind : "other"}`)}</div>}
    </div>
  )
}
