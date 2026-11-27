import { useOpenDecisions } from "@/data/sync"
/* Steve's decisions drawn with affectUI's Decision Card and Approval Card (components/affect).
 * One mapping from a Hub decision (Item + Wait) to the card's question, rows and own-words field, and back to the
 * Hub's choice, used in three places: in the conversation, in 待处理 / an item's page, and above the chat composer.
 * Decisions whose subject has a body of its own (mail drafts, flows, contacts, vault, memory) keep that body; above
 * the composer they offer one row that opens them. */
import { ApprovalCard, type ApprovalRisk } from "@/components/affect/approval-card"
import { DecisionCard as AuDecisionCard, type Decision, type DecisionAnswer, type DecisionCardLabels, type DecisionOption } from "@/components/affect/decision-card"
import { Markdown } from "@/components/markdown"
import { PermissionDetails } from "@/components/permission"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { TextArea } from "@/components/affect/text-area-clean"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuRadioGroup, DropdownMenuRadioItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { OutContext } from "@/channels/out-context"
import { handoffContent } from "@/channels/handoff-content"
import { HandoffPreview, ReplyDraft } from "@/channels/reply-preview"
import { MessageOpenInSourceApp } from "@/channels/open-in-app"
import { SendRecovery } from "@/channels/send-recovery"
import { outContext, type ROutContext } from "@/api/channels"
import { t as tr, useT } from "@/i18n"
import { FLOW_SUBJECTS, hasOptionIds, isModuleCard, isPeopleDecision, WITH_WORDS, WORDS_OPTIONS } from "@/decision-options"
import { botName, channelAnswer, decide, dismiss, FAIL_CHOICE, optionLabel, decideCards, GITHUB_PR_MERGE, actOnItem, SNOOZE_KEYS, snooze, snoozeLabel, st, WT, type Item, type Wait } from "@/store"
import { permissionCommand, Pill } from "@/ui"
import { AlarmClockIcon, BanIcon, BellOffIcon, EllipsisIcon, SendIcon } from "lucide-react"
import { useEffect, useRef, useState, type ReactNode } from "react"

/** the order of a failure card's choices; "continue" is the own-words field (a note to go on with) */
const FAIL_ORDER = ["wait", "retry", "release_live", "release_retry", "release_waive", "delivered", "tosteve", "drop", "look", "continue"]

/** a decision whose subject draws its own body (Steve's card keeps it); every `approve` decision uses affectUI's Approval Card */
export const hasOwnBody = (w: Wait) => FLOW_SUBJECTS.includes(w.subject) || isPeopleDecision(w) || w.subject === "mail_draft" || w.subject === "no_executor" || isModuleCard(w.subject)
export const isToolCall = (w: Wait) => w.type === "approve" && w.subject === "tool_call"
export const isChannelApproval = (w: Wait) => w.type === "approve" && w.subject === "chan_message"
export const isPlainApproval = (w: Wait) => w.type === "approve" && !isToolCall(w) && !isChannelApproval(w)

const toolOptions = (w: Wait) => {
  const opts = w.options.map((o) => { const [id, name, kind] = o.split("|"); return { id, name, kind } })
  return { always: opts.find((o) => o.kind === "allow_always"), once: opts.find((o) => o.kind === "allow_once") }
}
const kindLabel = (w: Wait) => (w.subject === "tool_call" ? tr("core:wait.tool") : w.subject === "release" ? tr("core:wait.release") : w.subject === "no_executor" ? tr("core:wait.noExecutor") : w.subject === GITHUB_PR_MERGE ? tr("core:wait.prMerge") : WT[w.type])
export const busyOf = (w: Wait) => st.pendingActions.has("decision:" + w.decision) || st.submittedChannelDecisions.has(w.decision) || st.inactiveChannelDecisions.has(w.decision) || !!channelAnswer(w)

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
        {w.subject !== "session_start" && !(w.subject === "mail_draft" && w.options.includes("confirmed_not_sent")) && <DropdownMenuItem disabled={busy} onClick={() => void dismiss(i.id, w.decision)} data-dismiss={i.id}><BellOffIcon />{t(w.subject === "tool_call" ? "core:card.dismissTool" : "core:card.dismiss")}</DropdownMenuItem>}
        {i.id && <><DropdownMenuSeparator />
        <DropdownMenuItem variant="destructive" disabled={busy} onClick={() => actOnItem(i.id, "drop")} data-drop-item={i.id}><BanIcon />{t("core:card.dropItem")}</DropdownMenuItem></>}
      </DropdownMenuContent>
    </DropdownMenu>
  </span>
}

/** what the question is about: the conversation a reply goes to, the draft, the agent still working */
function Detail({ w, extra, reply = false }: { w: Wait; extra?: ReactNode; reply?: boolean }) {
  const t = useT()
  return <>
    {w.subject === "chan_message" && (w.type === "approve" || w.type === "fail") && <OutContext id={w.subjectId} />}
    {w.subject === "chan_message" && <MessageOpenInSourceApp message={w.subjectId} className="-ml-2 self-start" />}
    {w.subject === "session_start" && w.subjectId && <Button variant="outline" size="sm" data-session-config={w.subjectId} onClick={() => { const id = w.subjectId; if (id) void import("@/manage").then(({ editBot }) => editBot(id)) }}>{t("core:card.fixBotConfig")}</Button>}
    {/* the draft's headings are text: a "## 交付" never outranks the question above it */}
    {w.draft && !isToolCall(w) && !reply && (w.subject === "chan_message" ? <ReplyDraft text={w.draft} /> : <div className="max-h-72 overflow-auto break-words text-muted-foreground [&_:is(h1,h2,h3,h4,h5,h6)]:mt-0! [&_:is(h1,h2,h3,h4,h5,h6)]:mb-1! [&_:is(h1,h2,h3,h4,h5,h6)]:[font:inherit]! [&_:is(h1,h2,h3,h4,h5,h6)]:font-medium! [&_:is(h1,h2,h3,h4,h5,h6)]:text-foreground"><Markdown text={w.draft} /></div>)}
    {extra}
    {w.busy && <div role="status" className="flex items-center gap-1.5 type-footnote text-muted-foreground" data-agent-busy><span className="size-1.5 rounded-full bg-blue dot-running" aria-hidden />{t("core:card.agentBusy")}</div>}
    {st.pendingActions.has("decision:" + w.decision) && !channelAnswer(w) && <div role="status" className="type-footnote text-muted-foreground">{t("core:card.submitting")}</div>}
  </>
}

const CHANNEL_RESULTS = {
  yes: "channels:reply.queued", no: "core:decided.chan_message.no", to_item: "core:decided.chan_message.to_item",
  rewrite: "core:decided.chan_message.rewrite", done: "core:decided.chan_message.done",
  retry: "core:decided.chan_message.retry", discard: "core:decided.chan_message.discard", sent: "core:decided.chan_message.sent",
} as const
function ChannelResult({ w }: { w: Wait }) {
  const t = useT(), choice = channelAnswer(w)?.result?.choice
  const key = typeof choice === "string" && Object.hasOwn(CHANNEL_RESULTS, choice) ? CHANNEL_RESULTS[choice as keyof typeof CHANNEL_RESULTS] : "core:decided.handled"
  return <div role="status" className="type-footnote text-muted-foreground" data-reply-result={typeof choice === "string" ? choice : "submitted"}>{t(key)}</div>
}

/** Only an approval of a channel message can send. Editing is local; yes carries the reviewed text to the
 * same decision. An ask_owner question stays a question even when its prose happens to contain a reply. */
function ChannelReply({ i, w }: { i: Item; w: Wait }) {
  const t = useT(), busy = busyOf(w)
  const draft = w.draft ?? ""
  // Capture the current formal draft on entry, not at mount: the same decision can be edited elsewhere and undone.
  // Keep unsaved input against its base so a received update cannot silently replace either version.
  const [editor, setEditor] = useState<{ base: string; text: string } | null>(() => draft.trim() ? null : { base: draft, text: draft })
  const conflict = editor !== null && editor.base !== draft
  const [error, setError] = useState(false)
  const sending = useRef(false)
  const accepted = channelAnswer(w)
  const canSend = w.options.includes("yes")
  const submit = async (choice: string, body?: string) => {
    if (sending.current || busyOf(w) || !w.options.includes(choice)) return
    if (choice === "yes" && (conflict || !(body ?? draft).trim())) return
    sending.current = true
    setError(false)
    try { if (!await decide(i.id, choice, body, w.decision)) setError(true) }
    finally { sending.current = false }
  }
  return <div className="flex min-w-0 flex-col gap-3" data-channel-reply>
    {editor && canSend
      ? <TextArea label={t("channels:reply.draft")} value={editor.text} onChange={(e) => setEditor({ ...editor, text: e.target.value })} disabled={busy} minRows={3} maxRows={12} autoFocus data-reply-editor />
      : <div>
        <div className="mb-1 type-footnote font-medium">{t("channels:reply.draft")}</div>
        <div className="max-h-72 overflow-auto whitespace-pre-wrap break-words" data-reply-draft>{w.draft || t("channels:reply.noDraft")}</div>
      </div>}
    {!accepted && <>
      {conflict && canSend && <div className="flex flex-col gap-2 rounded-md border border-border p-3" data-reply-conflict>
        <div role="alert" className="type-footnote text-destructive">{t("channels:reply.draftChanged")}</div>
        <div className="type-footnote font-medium">{t("channels:reply.latestDraft")}</div>
        <div className="max-h-72 overflow-auto whitespace-pre-wrap break-words" data-reply-latest>{draft || t("channels:reply.noDraft")}</div>
        <div className="flex flex-wrap gap-2">
          <Button size="sm" variant="outline" disabled={busy} onClick={() => setEditor({ base: draft, text: draft })} data-reply-use-latest>{t("channels:reply.useLatest")}</Button>
          <Button size="sm" variant="outline" disabled={busy} onClick={() => setEditor({ ...editor, base: draft })} data-reply-keep-edits>{t("channels:reply.keepEdits")}</Button>
        </div>
      </div>}
      {canSend && <div className="type-footnote text-muted-foreground">{t("channels:reply.reviewNote")}</div>}
      <div className="flex flex-wrap gap-2">
        {canSend && <>
          <Button size="sm" disabled={busy || conflict || !(editor ? editor.text : draft).trim()} onClick={() => void submit("yes", editor ? editor.text.trim() : undefined)} data-approve data-reply-send>{t(editor ? "channels:reply.sendEdited" : "channels:reply.send")}</Button>
          {editor
            ? <Button size="sm" variant="ghost" disabled={busy} onClick={() => setEditor(null)} data-reply-cancel>{t("common:action.cancel")}</Button>
            : <Button size="sm" variant="outline" disabled={busy} onClick={() => setEditor({ base: draft, text: draft })} data-reply-edit>{t("channels:reply.edit")}</Button>}
        </>}
        {w.options.filter((o) => o !== "yes").map((o) => <Button key={o} size="sm" variant="outline" disabled={busy} onClick={() => void submit(o)} data-reply-choice={o} data-rewrite={o === "rewrite" ? "" : undefined}>{o === "no" ? t("channels:reply.noReply") : optionLabel(w.subject, o)}</Button>)}
      </div>
    </>}
    {error && <div role="alert" className="type-footnote text-destructive" data-reply-error>{t("channels:reply.submitFailed")}</div>}
    {accepted && <ChannelResult w={w} />}
  </div>
}

type Mapped = { d: Decision; answer: (a: DecisionAnswer) => void }

/** a Hub decision as the card's question, rows and field, and the way back to the Hub's choice.
 *  `where`: "inline" in the conversation or a list, "composer" above the chat composer (subjects with their own body open) */
export function mapDecision(i: Item, w: Wait, where: "inline" | "composer", showItem = true): Mapped | null {
  const act = (c: string, text?: string) => void decide(i.id, c, text, w.decision)
  const opt = (id: string, title: string, attrs?: Record<string, string | undefined>): DecisionOption => ({ id, title, attrs })
  const base: Decision = {
    id: w.decision, question: w.q, badge: kindLabel(w), busy: busyOf(w) && !channelAnswer(w),
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
  if (channelAnswer(w) && w.type !== "approve") return {
    d: { ...base, detail: <Detail w={w} extra={<ChannelResult w={w} />} />, options: [], own: false }, answer: () => {},
  }
  if (w.type === "accept") {
    const rows = [opt("pass", tr("core:card.pass"), { "data-pass": "" })]
    return {
      d: { ...base, options: rows, own: { placeholder: tr("core:card.reworkPlaceholder", { bot: botName(i.b) }), send: tr("core:card.sendRework"), attrs: { "data-rework": "" } } },
      answer: (a) => pick(a, { pass: () => act("pass") }, (x) => act("rework", x)),
    }
  }
  if (w.type === "approve") {
    if (w.subject === "chan_message") return {
      d: { ...base, detail: <Detail w={w} reply extra={<ChannelReply key={w.decision} i={i} w={w} />} />, options: [], own: false },
      answer: () => {},
    }
    const rows = [opt("yes", tr(w.subject === "release" ? "core:card.approveRelease" : w.subject === GITHUB_PR_MERGE ? "core:card.approveMerge" : "core:card.approve"), { "data-approve": "" }),
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
  // SendRecovery owns recovery.authorization_url -> Proposal -> apply/undo; retry stays a separate decision.
  if (w.type === "fail" && w.subject === "chan_message") return {
    d: { ...base, detail: <><SendRecovery key={w.decision} i={i} w={w} /></>,
      options: w.options.filter((o) => o !== "rewrite").map((o) => opt(o, o === "retry" && w.options.includes("rewrite") ? tr("core:sendRecovery.retry") : optionLabel(w.subject, o), { "data-fail": o })), own: false },
    answer: (a) => typeof a === "object" && a.option && w.options.includes(a.option) && a.option !== "rewrite" && act(a.option),
  }
  if (w.type === "fail" && hasOptionIds(w.subject)) return {
    /* a module's failure card (a reply that didn't go out): its own options, in the Hub's order, each by its label */
    d: { ...base, options: w.options.map((o) => opt(o, optionLabel(w.subject, o), { "data-fail": o })), own: false },
    answer: (a) => typeof a === "object" && a.option && act(a.option),
  }
  if (w.type === "fail") {
    const botOpts = w.options.filter((o) => o.startsWith("bot:"))
    const known = w.options.filter((o) => FAIL_ORDER.includes(o)).sort((a, b) => FAIL_ORDER.indexOf(a) - FAIL_ORDER.indexOf(b))
    const go = known.includes("continue")
    const beforeTosteve = known.filter((o) => o !== "continue" && FAIL_ORDER.indexOf(o) < FAIL_ORDER.indexOf("tosteve"))
    const fromTosteve = known.filter((o) => o !== "continue" && FAIL_ORDER.indexOf(o) >= FAIL_ORDER.indexOf("tosteve"))
    const ordered = [...beforeTosteve, ...botOpts, ...fromTosteve]
    const labelFor = (o: string) => o.startsWith("bot:")
      ? tr("core:fail.handoffBot", { bot: botName(o.slice(4)) })
      : w.subject === "session_start" && o === "retry"
        ? tr("core:card.retrySession")
        : FAIL_CHOICE[o]
    return {
      d: { ...base, options: ordered.map((o) => opt(o, labelFor(o), { "data-fail": o })), own: go ? { placeholder: tr("core:card.nextPlaceholder"), send: FAIL_CHOICE.continue, required: false, attrs: { "data-fail-note": "" }, sendAttrs: { "data-fail": "continue" } } : false },
      answer: (a) => { if (typeof a !== "object") return; if (a.option) act(a.option); else if (go) act("continue", a.text) },
    }
  }
  if (w.type === "ask" && w.subject === "design_implementation") {
    /* the typed note goes with every choice; a revision needs one. The owner turned the item's design gate on,
     * and only a person answers it: automatic permissions never bypass this card */
    const [impl, keep, revise] = w.options
    const rows = [impl && opt(impl, tr("core:card.designImplement"), { "data-design-choice": "0" }), keep && opt(keep, tr("core:card.designKeep"), { "data-design-choice": "1" })].filter(Boolean) as DecisionOption[]
    const note = <div className="type-footnote text-muted-foreground" data-design-gate-note>{tr("core:card.designGateNote")}</div>
    return {
      d: { ...base, detail: <Detail w={w} extra={note} />, options: rows, own: revise ? { placeholder: tr("core:card.designFeedback"), send: tr("core:card.designRevise"), attrs: { "data-design-feedback": "" }, sendAttrs: { "data-design-choice": "2" } } : false },
      answer: (a) => { if (typeof a !== "object") return; if (a.option) act(a.option, a.text ?? ""); else if (revise && a.text) act(revise, a.text) },
    }
  }
  if (w.type === "ask" && hasOptionIds(w.subject)) {
    /* a module's question (a message handed over to you): the choice sent is the option's id, shown by its label; the
     * option that takes words (how to reply) is the field, its label on the send
     * button; an option that sends your words as they are (按原文发) takes what you typed along. The card's first line
     * is its question; the lines under it (why you, the conversation) and the candidate draft are its detail */
    const words = (WORDS_OPTIONS[w.subject] ?? []).find((o) => w.options.includes(o)), withWords = WITH_WORDS[w.subject] ?? []
    const [head, ...rest] = w.q.split("\n")
    const channel = w.subject === "chan_message"
    const detail = <Detail w={{ ...w, draft: undefined }} extra={channel ? <HandoffPreview key={w.decision} question={w.q} draft={w.draft} /> : <>
      {rest.length > 0 && <div className="whitespace-pre-line type-footnote text-muted-foreground" data-card-context>{rest.join("\n")}</div>}
      {w.draft && <div className="flex flex-col gap-1" data-card-draft><span className="type-footnote text-muted-foreground">{tr("core:card.candidateDraft")}</span><div className="break-words whitespace-pre-line">{w.draft}</div></div>}
    </>} />
    return {
      d: { ...base, question: (channel && handoffContent(w.q)?.question) || head, detail, options: w.options.filter((o) => o !== words).map((o) => opt(o, optionLabel(w.subject, o), { "data-option": o })),
        own: words ? { placeholder: tr("core:card.instructPlaceholder"), send: optionLabel(w.subject, words), attrs: { "data-option-words": words }, sendAttrs: { "data-option": words } } : false },
      answer: (a) => {
        if (typeof a !== "object") return
        if (a.option && withWords.includes(a.option)) { if (a.text?.trim()) act(a.option, a.text.trim()) }
        else if (a.option) act(a.option)
        else if (words && a.text?.trim()) act(words, a.text.trim())
      },
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
    attrs={{ "data-decision": w.type, "data-decision-wrap": i.id || w.decision, "data-decision-id": w.decision, "aria-busy": busy ? "true" : undefined }}
  />
}

/** Outbound channel-message approval: uses affectUI ApprovalCard so send approvals are visually distinct from ordinary DecisionCards (#213). */
export function ChannelApproval({ i, w, showItem }: { i: Item; w: Wait; showItem: boolean }) {
  const t = useT()
  const [ctx, setCtx] = useState<ROutContext | "loading" | "unavailable">(() => (w.subjectId ? "loading" : "unavailable"))
  useEffect(() => {
    if (!w.subjectId) { setCtx("unavailable"); return }
    let live = true
    setCtx("loading")
    outContext(w.subjectId).then((x) => { if (live) setCtx(x) }, () => { if (live) setCtx("unavailable") })
    return () => { live = false }
  }, [w.subjectId])
  const ready = typeof ctx === "object" ? ctx : null
  const busy = busyOf(w) && !channelAnswer(w)
  const risk: ApprovalRisk = ready?.space.external ? "high" : ready?.as_you ? "medium" : "low"
  return <ApprovalCard
    className="steve-ap anim-in"
    icon={<SendIcon className="size-4" />}
    risk={ready ? risk : undefined}
    title={w.q}
    description={showItem ? <TaskLink i={i} w={w} /> : botName(w.by === "steve" ? "steve" : w.by)}
    label={<span className="flex w-full min-w-0 items-center justify-between gap-2"><span className="truncate">{kindLabel(w)}</span><CardTools i={i} w={w} /></span>}
    preview={<div className="flex min-w-0 flex-col gap-2">
      {w.escalated && <div><Pill tone="attention" data-draft-escalated>{t("channels:reply.escalated")}</Pill></div>}
      <OutContext context={ctx} />
      <MessageOpenInSourceApp message={w.subjectId} provider={ready?.open_provider} className="-ml-2 self-start" />
      {w.busy && <div role="status" className="flex items-center gap-1.5 type-footnote text-muted-foreground" data-agent-busy><span className="size-1.5 rounded-full bg-blue dot-running" aria-hidden />{t("core:card.agentBusy")}</div>}
      {st.pendingActions.has("decision:" + w.decision) && !channelAnswer(w) && <div role="status" className="type-footnote text-muted-foreground">{t("core:card.submitting")}</div>}
    </div>}
    status="pending"
    footer={<div className="w-full min-w-0"><ChannelReply key={w.decision} i={i} w={w} /></div>}
    attrs={{ "data-decision": w.type, "data-decision-wrap": i.id || w.decision, "data-decision-id": w.decision, "aria-busy": busy ? "true" : undefined }}
  />
}

/** Non-tool, non-channel approval (release, PR merge, external access): rendered as an ApprovalCard with optional reject reason (#213). */
export function PlainApproval({ i, w, showItem }: { i: Item; w: Wait; showItem: boolean }) {
  const t = useT()
  const [reason, setReason] = useState("")
  const busy = busyOf(w)
  const risk: ApprovalRisk = w.subject === "release" || w.subject === GITHUB_PR_MERGE ? "high" : "medium"
  const approveText = t(w.subject === "release" ? "core:card.approveRelease" : w.subject === GITHUB_PR_MERGE ? "core:card.approveMerge" : "core:card.approve")
  const submit = (choice: string, text?: string) => { if (!busy) void decide(i.id, choice, text, w.decision) }
  return <ApprovalCard
    className="steve-ap anim-in"
    risk={risk}
    title={w.q}
    description={showItem ? <TaskLink i={i} w={w} /> : botName(w.by === "steve" ? "steve" : w.by)}
    label={<span className="flex w-full min-w-0 items-center justify-between gap-2"><span className="truncate">{kindLabel(w)}</span><CardTools i={i} w={w} /></span>}
    preview={<div className="flex min-w-0 flex-col gap-2 empty:hidden [&>:empty]:hidden"><Detail w={w} /></div>}
    status="pending"
    footer={<div className="flex w-full min-w-0 flex-col gap-2">
      <div className="flex min-w-0 flex-wrap items-center gap-2">
        <Input className="min-w-40 flex-1" value={reason} disabled={busy} placeholder={t("core:card.rejectPlaceholder")}
          onChange={(e) => setReason(e.target.value)}
          onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); submit("no", reason.trim() || undefined) } }}
          data-reject-reason />
        <Button size="sm" variant="outline" disabled={busy} onClick={() => submit("no", reason.trim() || undefined)} data-reject>{t("core:card.reject")}</Button>
        <Button size="sm" disabled={busy} onClick={() => submit("yes")} data-approve>{approveText}</Button>
      </div>
    </div>}
    attrs={{ "data-decision": w.type, "data-decision-wrap": i.id || w.decision, "data-decision-id": w.decision, "aria-busy": busy ? "true" : undefined }}
  />
}

/** one decision in place (the conversation, 待处理, an item's page): the Decision Card without its pager or Later */
export function InlineDecision({ m }: { m: Mapped }) {
  return <AuDecisionCard className="steve-dc anim-in" decisions={[m.d]} onDecide={(_, a) => m.answer(a)} hold later={false} labels={dcLabels()} />
}

/** above the chat composer: at most 1 open Decide card directly blocking the current conversation,
 *  rendered directly without a folded multi-count yellow bar (sidebar 待我决定 owns the global count). */
export function ComposerDecisions({
  reveal,
  skip,
  focus,
  threadItems,
}: {
  reveal?: number
  skip?: Set<string>
  focus?: string | null
  threadItems?: Set<string>
}) {
  useT()
  const open = decideCards(useOpenDecisions()).filter(({ w }) => !skip?.has(w.decision))
  const direct = focus || threadItems
    ? open.filter(({ i, w }) => (!!focus && i.id === focus) || (!!i.id && !!threadItems?.has(i.id)) || (!i.id && !!w.session))
    : open
  const blocking = focus
    ? [...direct.filter(({ i }) => i.id === focus), ...direct.filter(({ i }) => i.id !== focus)]
    : direct
  const mapped = blocking.slice(0, 1).map(({ i, w }) => mapDecision(i, w, "composer", true)).filter(Boolean) as Mapped[]
  if (!mapped.length) return null
  const first = mapped[0]
  return <div className="mb-2" data-pending-strip>
    <AuDecisionCard className="steve-dc" decisions={[first.d]} onDecide={(_, a) => first.answer(a)} hold later={false} reveal={reveal} labels={dcLabels()} />
  </div>
}
