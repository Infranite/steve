/* Steve's decisions drawn with affectUI's Decision Card and Approval Card (components/affect).
 * One mapping from a Hub decision (Item + Wait) to the card's question, rows and own-words field, and back to the
 * Hub's choice, used in three places: in the conversation, in 待处理 / an item's page, and above the chat composer.
 * Decisions whose subject has a body of its own (mail drafts, flows, contacts, vault, memory) keep that body; above
 * the composer they offer one row that opens them. */
import { ApprovalCard } from "@/components/affect/approval-card"
import { DecisionCard as AuDecisionCard, type Decision, type DecisionAnswer, type DecisionCardLabels, type DecisionOption } from "@/components/affect/decision-card"
import { Markdown } from "@/components/markdown"
import { PermissionDetails } from "@/components/permission"
import { Button } from "@/components/ui/button"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuRadioGroup, DropdownMenuRadioItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { OutContext } from "@/channels/out-context"
import { t as tr, useT } from "@/i18n"
import { FLOW_SUBJECTS } from "@/auto/decision"
import { MODULE_CARDS } from "@/vault/decisions"
import { isPeopleDecision } from "@/people/pending"
import { acceptChoosesRelease, botName, decide, dismiss, FAIL_CHOICE, pendingCards, setSt, SNOOZE_KEYS, snooze, snoozeLabel, st, WT, type Item, type Wait } from "@/store"
import { permissionCommand } from "@/ui"
import { YoloBadge } from "@/yolo/badges"
import { AlarmClockIcon, BanIcon, BellOffIcon, EllipsisIcon } from "lucide-react"
import type { ReactNode } from "react"

/** the order of a failure card's choices; "continue" is the own-words field (a note to go on with) */
const FAIL_ORDER = ["retry", "release_live", "release_retry", "release_waive", "delivered", "tosteve", "drop", "look", "continue"]

/** a decision whose subject draws its own body (Steve's card keeps it); a tool call is an Approval Card */
export const hasOwnBody = (w: Wait) => FLOW_SUBJECTS.includes(w.subject) || (w.type === "confirm" && w.subject === "mem") || isPeopleDecision(w) || w.subject === "mail_draft" || !!MODULE_CARDS[w.subject]
export const isToolCall = (w: Wait) => w.type === "approve" && w.subject === "tool_call"

const toolOptions = (w: Wait) => {
  const opts = w.options.map((o) => { const [id, name, kind] = o.split("|"); return { id, name, kind } })
  return { always: opts.find((o) => o.kind === "allow_always"), once: opts.find((o) => o.kind === "allow_once") }
}
const kindLabel = (w: Wait) => (w.subject === "tool_call" ? tr("core:wait.tool") : w.subject === "release" ? tr("core:wait.release") : WT[w.type])
export const busyOf = (w: Wait) => st.pendingActions.has("decision:" + w.decision)

/** the card's labels in the UI language ("later" folds the card here; above the composer it reads Hide, next to the Hub's remind-me-later) */
export const dcLabels = (): Partial<DecisionCardLabels> => ({
  needsYou: tr("core:card.needsYou"), later: tr("core:card.later"), show: tr("core:card.show"), recommended: tr("core:card.recommended"),
  approve: tr("core:card.approve"), decline: tr("core:card.reject"), own: tr("core:card.own"), send: tr("core:card.send"), answered: tr("core:card.answered"),
  previous: tr("core:card.prevDecision"), next: tr("core:card.nextDecision"), allDone: tr("core:card.allDone"),
  waiting: (n) => tr("pages:chat.pending", { count: n }), of: (i, n) => tr("core:card.pageOf", { i, n }),
})

/** where the decision belongs: the item, or the session of a decision without one */
function TaskLink({ i, w }: { i: Item; w: Wait }) {
  const t = useT()
  if (i.id) return <a href={"#/items/" + i.id} className="hover:text-foreground hover:underline" onClick={(e) => e.stopPropagation()}><span className="font-mono tabular-nums">{i.id}</span> {i.t}</a>
  if (w.session) return <a href={"#/sessions/" + w.session} className="hover:text-foreground hover:underline" data-orphan-session={w.session}>{t("core:card.sessionTitle", { title: i.t })}</a>
  return null
}

/** the head's own controls: who waits since when, remind me later, withdraw the card or give up the item */
export function CardTools({ i, w }: { i: Item; w: Wait }) {
  const t = useT(), busy = busyOf(w)
  return <span className="flex min-w-0 items-center gap-0.5 type-footnote text-muted-foreground">
    <span className="mr-1 max-w-48 truncate max-sm:hidden">{t("core:card.waited", { bot: botName(w.by === "steve" ? "steve" : w.by), since: w.since })}</span>
    {i.id && <DropdownMenu>
      <DropdownMenuTrigger render={<Button variant="ghost" size="icon-xs" aria-label={t("core:card.snooze")} className="text-muted-foreground" data-snooze={i.id} />}><AlarmClockIcon /></DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-40">
        <DropdownMenuRadioGroup value="" onValueChange={(v) => snooze(i.id, v as (typeof SNOOZE_KEYS)[number])}>{SNOOZE_KEYS.map((x) => <DropdownMenuRadioItem key={x} value={x} data-snooze-opt={x}>{snoozeLabel(x)}</DropdownMenuRadioItem>)}</DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>}
    <DropdownMenu>
      <DropdownMenuTrigger render={<Button variant="ghost" size="icon-xs" aria-label={t("common:action.more")} className="text-muted-foreground" data-decision-more={i.id} />}><EllipsisIcon /></DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-52">
        {!(w.subject === "mail_draft" && w.options.includes("confirmed_not_sent")) && <DropdownMenuItem disabled={busy} onClick={() => void dismiss(i.id, w.decision)} data-dismiss={i.id}><BellOffIcon />{t(w.subject === "tool_call" ? "core:card.dismissTool" : "core:card.dismiss")}</DropdownMenuItem>}
        {i.id && <><DropdownMenuSeparator />
        <DropdownMenuItem variant="destructive" disabled={busy} onClick={() => setSt(i.id, "drop")} data-drop-item={i.id}><BanIcon />{t("core:card.dropItem")}</DropdownMenuItem></>}
      </DropdownMenuContent>
    </DropdownMenu>
  </span>
}

/** what the question is about: the conversation a reply goes to, the draft, the YOLO note, the agent still working */
function Detail({ w, extra }: { w: Wait; extra?: ReactNode }) {
  const t = useT()
  return <>
    {w.subject === "chan_message" && (w.type === "approve" || w.type === "fail") && <OutContext id={w.subjectId} />}
    {w.draft && !isToolCall(w) && <div className="max-h-72 overflow-auto break-words text-muted-foreground"><Markdown text={w.draft} /></div>}
    {extra}
    <YoloBadge decision={w.decision} subject={w.subject} />
    {w.busy && <div role="status" className="flex items-center gap-1.5 type-footnote text-muted-foreground" data-agent-busy><span className="size-1.5 rounded-full bg-blue dot-running" aria-hidden />{t("core:card.agentBusy")}</div>}
    {busyOf(w) && <div role="status" className="type-footnote text-muted-foreground">{t("core:card.submitting")}</div>}
  </>
}

type Mapped = { d: Decision; answer: (a: DecisionAnswer) => void }

/** a Hub decision as the card's question, rows and field, and the way back to the Hub's choice.
 *  `where`: "inline" in the conversation or a list, "composer" above the chat composer (subjects with their own body open) */
export function mapDecision(i: Item, w: Wait, where: "inline" | "composer", showItem = true): Mapped | null {
  const act = (c: string, text?: string) => void decide(i.id, c, text, w.decision)
  const opt = (id: string, title: string, attrs?: Record<string, string | undefined>): DecisionOption => ({ id, title, attrs })
  const base: Decision = {
    id: w.decision, question: w.q, badge: kindLabel(w), busy: busyOf(w),
    task: showItem || where === "composer" ? <TaskLink i={i} w={w} /> : undefined,
    tools: <CardTools i={i} w={w} />,
    detail: <Detail w={w} />,
    attrs: { "data-decision": w.type, "data-decision-wrap": i.id || w.decision, "data-decision-id": w.decision },
  }
  const pick = (a: DecisionAnswer, rows: Record<string, (text?: string) => void>, own?: (text: string) => void) => {
    if (typeof a !== "object") return
    if (a.option) rows[a.option]?.(a.text)
    else if (a.text != null) own?.(a.text)
  }
  if (hasOwnBody(w)) {
    if (where === "inline") return null
    const href = i.id ? "#/items/" + i.id : "#/inbox"
    return { d: { ...base, detail: <div className="type-footnote text-muted-foreground">{tr("core:card.openToAnswerSub")}</div>, options: [opt("open", tr("core:card.openToAnswer"))], own: false }, answer: () => { location.hash = href } }
  }
  if (isToolCall(w)) {
    if (where === "inline") return null
    const { always, once } = toolOptions(w)
    const rows: DecisionOption[] = [...(once ? [opt(once.id, tr("core:card.allowOnce"), { "data-allow": "" })] : []), ...(always ? [opt(always.id, tr("core:card.allowAlways"))] : []), opt("deny", tr("core:card.deny"))]
    return { d: { ...base, question: tr("core:card.allowTool"), detail: <Detail w={w} extra={<PermissionDetails title={permissionCommand(w.q)} input={w.draft} />} />, options: rows, own: false }, answer: (a) => typeof a === "object" && a.option && act(a.option) }
  }
  if (w.type === "accept") {
    const release = acceptChoosesRelease(i)
    const rows = release
      ? [opt("pass-release", tr("core:card.passRelease"), { "data-pass": "", "data-pass-release": "" }), opt("pass-no-release", tr("core:card.passNoRelease"), { "data-pass-no-release": "" })]
      : [opt("pass", tr("core:card.pass"), { "data-pass": "" })]
    return {
      d: { ...base, options: rows, own: { placeholder: tr("core:card.reworkPlaceholder", { bot: botName(i.b) }), send: tr("core:card.sendRework"), attrs: { "data-rework": "" } } },
      answer: (a) => pick(a, { pass: () => act("pass"), "pass-release": () => act("pass", "release"), "pass-no-release": () => act("pass", "no-release") }, (x) => act("rework", x)),
    }
  }
  if (w.type === "approve") {
    const rows = [opt("yes", tr(w.subject === "release" ? "core:card.approveRelease" : "core:card.approve"), { "data-approve": "" }),
      ...(w.subject === "chan_message" && w.options.includes("rewrite") ? [opt("rewrite", tr("core:card.rewriteFirst"), { "data-rewrite": "" })] : [])]
    /* rejecting is the field: its button sends with or without a reason */
    return {
      d: { ...base, options: rows, own: { placeholder: tr("core:card.rejectPlaceholder"), send: tr("core:card.reject"), required: false, attrs: { "data-reject-reason": "" }, sendAttrs: { "data-reject": "" } } },
      answer: (a) => pick(a, { yes: () => act("yes"), rewrite: () => act("rewrite") }, (x) => act("no", x || undefined)),
    }
  }
  if (w.type === "propose") return {
    d: { ...base, options: [opt("yes", tr("core:card.start"), { "data-propose-yes": "" }), opt("no", tr("core:card.notNeeded"))], own: false },
    answer: (a) => pick(a, { yes: () => act("yes"), no: () => act("no") }),
  }
  if (w.type === "fail") {
    const os = w.options.filter((o) => FAIL_ORDER.includes(o)).sort((a, b) => FAIL_ORDER.indexOf(a) - FAIL_ORDER.indexOf(b))
    const go = os.includes("continue")
    return {
      d: { ...base, options: os.filter((o) => o !== "continue").map((o) => opt(o, FAIL_CHOICE[o], { "data-fail": o })), own: go ? { placeholder: tr("core:card.nextPlaceholder"), send: FAIL_CHOICE.continue, required: false, attrs: { "data-fail-note": "" }, sendAttrs: { "data-fail": "continue" } } : false },
      answer: (a) => { if (typeof a !== "object") return; if (a.option) act(a.option); else if (go) act("continue", a.text) },
    }
  }
  if (w.type === "ask" && w.subject === "design_implementation") {
    /* the typed note goes with every choice; a revision needs one */
    const [impl, keep, revise] = w.options
    const rows = [impl && opt(impl, tr("core:card.designImplement"), { "data-design-choice": "0" }), keep && opt(keep, tr("core:card.designKeep"), { "data-design-choice": "1" })].filter(Boolean) as DecisionOption[]
    return {
      d: { ...base, options: rows, own: revise ? { placeholder: tr("core:card.designFeedback"), send: tr("core:card.designRevise"), attrs: { "data-design-feedback": "" }, sendAttrs: { "data-design-choice": "2" } } : false },
      answer: (a) => { if (typeof a !== "object") return; if (a.option) act(a.option, a.text ?? ""); else if (revise && a.text) act(revise, a.text) },
    }
  }
  if (w.type === "ask") {
    const opts = w.opts ?? []
    return {
      d: { ...base, options: opts.map((o, n) => opt(String(n), o, { "data-answer-opt": o })), own: { placeholder: opts.length ? tr("core:card.own") : tr("core:card.answerPlaceholder"), send: opts.length ? undefined : tr("core:card.answer"), attrs: { "data-free-answer": "" }, sendAttrs: { "data-answer": "" } } },
      answer: (a) => { if (typeof a !== "object") return; if (a.option != null) act("answer", opts[Number(a.option)]); else if (a.text) act("answer", a.text) },
    }
  }
  return null
}

/** a tool permission: the command and its input, allow once, allow this kind always (when the harness offers it) or deny */
export function ToolApproval({ i, w, showItem }: { i: Item; w: Wait; showItem: boolean }) {
  const t = useT()
  const { always, once } = toolOptions(w), busy = busyOf(w)
  const answer = ({ always: a }: { always: boolean }) => { const o = a ? always ?? once : once ?? always; return o ? decide(i.id, o.id, undefined, w.decision) : undefined }
  return <ApprovalCard
    className="steve-ap" title={t("core:card.allowTool")} description={showItem ? <TaskLink i={i} w={w} /> : botName(w.by === "steve" ? "steve" : w.by)}
    label={<span className="flex w-full min-w-0 items-center justify-between gap-2"><span className="truncate">{kindLabel(w)}</span><CardTools i={i} w={w} /></span>}
    preview={<div className="flex min-w-0 flex-col gap-2 empty:hidden [&>:empty]:hidden"><PermissionDetails className="-m-3 rounded-none bg-transparent" title={permissionCommand(w.q)} input={w.draft} /><div className="flex flex-col gap-2 pt-3 empty:hidden"><Detail w={w} /></div></div>}
    status="pending" allowAlways={!!always && !!once} alwaysLabel={t("core:card.allowAlwaysCheck")}
    approveLabel={once ? t("core:card.allowOnce") : t("core:card.allowAlways")} denyLabel={t("core:card.deny")}
    resolvedLabels={{ approved: t("core:card.allowed"), denied: t("core:card.denied"), expired: t("core:card.expired") }}
    onApprove={busy ? undefined : answer} onDeny={() => { if (!busy) void decide(i.id, "deny", undefined, w.decision) }}
    approveAttrs={{ "data-allow": "" }} denyAttrs={{ "data-deny": "" }}
    attrs={{ "data-decision": w.type, "data-decision-wrap": i.id || w.decision, "data-decision-id": w.decision }}
  />
}

/** one decision in place (the conversation, 待处理, an item's page): the Decision Card without its pager or Later */
export function InlineDecision({ m }: { m: Mapped }) {
  return <AuDecisionCard className="steve-dc anim-in" decisions={[m.d]} onDecide={(_, a) => m.answer(a)} hold later={false} labels={dcLabels()} />
}

/** above the chat composer: every open decision in one card, paged, folded to a line until you open it */
export function ComposerDecisions({ reveal, skip }: { reveal?: number; skip?: Set<string> }) {
  useT()
  const cards = pendingCards().filter(({ w }) => !skip?.has(w.decision))
  const mapped = cards.map(({ i, w }) => mapDecision(i, w, "composer", true)).filter(Boolean) as Mapped[]
  if (!mapped.length) return null
  const by = new Map(mapped.map((m) => [m.d.id, m]))
  return <div className="mb-2" data-pending-strip>
    <AuDecisionCard className="steve-dc" decisions={mapped.map((m) => m.d)} onDecide={(id, a) => by.get(id)?.answer(a)} hold defaultFolded reveal={reveal} labels={{ ...dcLabels(), later: tr("core:card.hide") }} />
  </div>
}
