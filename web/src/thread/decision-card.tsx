import { useState, type ReactNode } from "react"
import { useT } from "@/i18n"
import { Card as UICard, CardAction, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card"
import { st, WT, decide, getItem, getSession, sessionCards, closeRel, type Item } from "@/store"
import { RelNote, StatusDot } from "@/ui"
import { FLOW_SUBJECTS, isModuleCard, isPeopleDecision } from "@/decision-options"
import { YoloBadge } from "@/yolo/badges"
import { MemConfirm, memAsk } from "@/knowledge/parts"
import { FlowDecisionBody } from "@/auto/decision"
import { MODULE_CARDS } from "@/vault/decisions"
import { MailDraftCard } from "@/mail/draft"
import { CardTools, InlineDecision, isToolCall, mapDecision, ToolApproval } from "@/thread/decide"
import { PeopleDecisionBody } from "@/people/pending"

export function DecisionCard({ i, w = i.wait!, showItem = true }: { i: Item; w?: Item["wait"] & {}; showItem?: boolean }) {
  const t = useT()
  const [localBusy, setBusy] = useState(false)
  const busy = localBusy || st.pendingActions.has("decision:" + w.decision)
  const act = async (c: string, t?: string) => { if (busy) return; setBusy(true); try { await decide(i.id, c, t, w.decision) } finally { setBusy(false) } }
  const head = (title: ReactNode) => (
    <CardHeader className="flex min-w-0 flex-col gap-2 pt-4 pb-3">
      <div className="flex w-full min-w-0 flex-wrap items-center justify-between gap-x-3 gap-y-1">
      <CardDescription className="flex min-w-0 max-w-full items-center gap-2 type-footnote">
        <StatusDot s={w.type === "propose" ? "prop" : w.type === "fail" ? "fail" : "wait"} /><span className="shrink-0 whitespace-nowrap font-medium text-foreground/80">{w.subject === "tool_call" ? t("core:wait.tool") : w.subject === "release" ? t("core:wait.release") : WT[w.type]}</span>
        {showItem && (i.id ? <><span className="text-tertiary">·</span><a href={"#/items/" + i.id} className="min-w-0 truncate hover:text-foreground hover:underline"><span className="font-mono tabular-nums">{i.id}</span> {i.t}</a></>
          : w.session ? <><span className="text-tertiary">·</span><a href={"#/sessions/" + w.session} className="min-w-0 truncate hover:text-foreground hover:underline" data-orphan-session={w.session}>{t("core:card.sessionTitle", { title: i.t })}</a></> : null)}
      </CardDescription>
      {/* who waits since when, remind me later, withdraw the card or give up the item */}
      <CardAction className="flex max-w-full items-center"><CardTools i={i} w={w} /></CardAction>
      </div>
      {title && <CardTitle className="col-span-full min-w-0 break-words type-headline">{title}</CardTitle>}
      <YoloBadge decision={w.decision} subject={w.subject} />
      {w.busy && <div role="status" className="col-span-full flex items-center gap-1.5 type-footnote text-muted-foreground" data-agent-busy><StatusDot s="run" className="size-1.5" />{t("core:card.agentBusy")}</div>}
    </CardHeader>
  )
  const foot = (c: ReactNode) => <CardFooter className="flex-wrap justify-end gap-2 bg-muted/50 px-4 py-3! dark:bg-muted/40">{c}</CardFooter>
  let body: ReactNode = null
  if (FLOW_SUBJECTS.includes(w.subject)) body = <FlowDecisionBody w={w} act={(c) => void act(c)} busy={busy} head={head} foot={foot} />
  else if (w.type === "confirm" && w.subject === "mem") body = <>{head(memAsk(w.q))}<MemConfirm q={w.q} draft={w.draft} busy={busy} act={(c, t) => void act(c, t)} /></>
  else if (isPeopleDecision(w)) body = <>{head(w.q)}<PeopleDecisionBody w={w} busy={busy} act={(c) => void act(c)} /></>
  else if (w.subject === "mail_draft") body = <MailDraftCard w={w} act={act} busy={busy} head={head} foot={foot} />
  else if (isModuleCard(w.subject)) body = MODULE_CARDS[w.subject]({ w, act, busy, head, foot })
  /* everything else is affectUI's Decision Card (a tool permission its Approval Card), wired to the same answers */
  else if (isToolCall(w)) return <ToolApproval i={i} w={w} showItem={showItem} />
  else { const m = mapDecision(i, w, "inline", showItem); return m ? <InlineDecision m={m} /> : null }
  return <UICard size="sm" className="gap-0 py-0 anim-in" data-decision={w.type} data-decision-wrap={i.id || w.decision} aria-busy={busy}>{busy && <div role="status" className="px-4 py-2 type-footnote text-muted-foreground">{t("core:card.submitting")}</div>}{body}</UICard>
}

/** decisions the session waits on, answerable here like in 待处理 (also for sessions without an item) */
export function SessionCards({ k }: { k: string }) {
  const cards = sessionCards(k)
  /* after the release card is answered, what became of it stays under the conversation */
  const item = getSession(k)?.item, it = item ? getItem(item) : undefined, rel = it?.rel
  if (!cards.length && !rel) return null
  return <div className="mt-6 flex flex-col gap-3" data-session-cards={k}>{rel && <RelNote rel={rel} box={rel.tone !== "ok"} onClose={rel.tone === "fail" && it ? () => closeRel(it) : undefined} />}{cards.map(({ i, w }) => <DecisionCard key={w.decision} i={i} w={w} showItem={!!i.id} />)}</div>
}
