/* What an outbound message answers, above its draft on the approval card and in-conversation draft:
 * who receives it (chat, audience, speaker, [AI] mark), why Steve drafted it, disclosure status,
 * and the message it replies to (with earlier turns folded). */
import { useEffect, useState } from "react"
import { outContext, type ROutContext } from "@/api/channels"
import { useT } from "@/i18n"
import { OriginalMessage } from "./reply-preview"

/* the space kinds this card names; others show the Hub's kind as is */
const KINDS = ["group", "topic_group", "channel", "dm", "self_dm"] as const
type Kind = (typeof KINDS)[number]
const known = (k: string): k is Kind => (KINDS as readonly string[]).includes(k)

const DISCLOSURE_KEY = {
  pass: "channels:out.disclosurePass",
  rewritten: "channels:out.disclosureRewritten",
  drafted: "channels:out.disclosureDrafted",
  checking: "channels:out.disclosureChecking",
  unchecked: "channels:out.disclosureUnchecked",
  leak: "channels:out.disclosureLeak",
} as const

export function OutContext({ id, context }: { id?: string; context?: ROutContext | null }) {
  const t = useT()
  const [c, setC] = useState<ROutContext | null>(null)
  const [open, setOpen] = useState(false)
  const [loaded, setLoaded] = useState<string | undefined>()
  useEffect(() => { if (!id || context !== undefined) return; let live = true; setC(null); setLoaded(undefined); setOpen(false); outContext(id).then((x) => { if (live) { setC(x); setLoaded(id) } }, () => { if (live) { setC(null); setLoaded(id) } }); return () => { live = false } }, [id, context])
  const shown = context === undefined ? (loaded === id ? c : null) : context
  if (!shown) return context !== undefined ? null : <div role="status" className="type-footnote text-muted-foreground" data-out-context-state={id && loaded !== id ? "loading" : "unavailable"}>{t(id && loaded !== id ? "common:state.loading" : "channels:reply.contextUnavailable")}</div>
  const aiMark = shown.as_you ? (shown.ai_mark ?? true) : false
  const who = shown.as_you
    ? t(aiMark ? "channels:out.asYou" : "channels:out.asYouNoAi")
    : shown.persona ? t("channels:out.asPersona", { persona: shown.persona }) : t("channels:out.asBot")
  const kind = shown.space.kind
  const members = shown.recipient?.member_count ?? shown.space.member_count
  const verdict = shown.disclosure?.verdict
  const discKey = verdict && Object.hasOwn(DISCLOSURE_KEY, verdict) ? DISCLOSURE_KEY[verdict as keyof typeof DISCLOSURE_KEY] : undefined
  return (
    <div className="flex min-w-0 flex-col gap-2 type-callout" data-out-context>
      <div className="flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-0.5 type-footnote text-muted-foreground [overflow-wrap:anywhere]" data-out-recipient>
        <span className="text-tertiary">{t("channels:out.toLabel")}</span>
        <button type="button" className="min-w-0 text-left font-medium text-foreground/85 hover:underline" onClick={() => { location.hash = "#/channels/space/" + shown.space.id }}>{shown.space.name}</button>
        <span>{known(kind) ? t(`channels:out.kind.${kind}`) : kind}</span>
        {!["dm", "self_dm"].includes(kind) && !!members && <span>{t("channels:bits.members", { count: members })}</span>}
        {shown.space.external && <span className="font-medium text-attention">{t("channels:word.externalMembers")}</span>}
        <span className="text-tertiary">·</span><span data-out-speaker>{who}</span>
        {discKey && <><span className="text-tertiary">·</span><span className={verdict === "leak" || verdict === "drafted" ? "font-medium text-attention" : verdict === "pass" ? "text-success" : "text-muted-foreground"} data-out-disclosure={verdict}>{t(discKey)}</span></>}
      </div>
      {shown.why && <div className="type-footnote text-muted-foreground [overflow-wrap:anywhere]" data-out-why>
        <span className="font-medium text-foreground/75">{t("channels:out.whyLabel")}</span>{shown.why}
      </div>}
      {shown.reply_to ? <>
        <div data-out-reply-to><OriginalMessage sender={shown.reply_to.sender} text={shown.reply_to.text} /></div>
        {shown.before.length > 0 && (open
          ? <div className="flex flex-col gap-1 border-l-2 pl-2.5 type-footnote text-muted-foreground anim-in" data-out-before>{shown.before.map((b, n) => <div key={n} className="whitespace-pre-wrap [overflow-wrap:anywhere]"><span className="font-medium">{t("channels:out.sender", { name: b.sender })}</span>{b.text ?? t("channels:out.cleaned")}</div>)}</div>
          : <button type="button" className="self-start type-footnote text-blue hover:underline" onClick={() => setOpen(true)}>{t("channels:out.before", { count: shown.before.length })}</button>)}
      </> : <div className="type-footnote text-muted-foreground">{t(`channels:out.direct.${known(kind) ? kind : "other"}`)}</div>}
    </div>
  )
}
