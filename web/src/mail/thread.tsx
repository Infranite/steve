/* 往来页 (email.md §11): `#/mail/t/:id`. Messages oldest first; the last one open, the others one line each (click to
 * open). Plain text by default; “显示原格式” shows the original HTML in a sandboxed iframe (no scripts, no same-origin)
 * with remote images blocked until “显示图片” (?images=1). Attachments are landed as files first, then opened.
 * Actions: 让邮件 Bot 起草 (with an instruction), 自己回复, 转成事项, 加进知识库, 归档, 静音. Drafts use the shared card. */
import { useEffect, useRef, useState } from "react"
import { ArchiveIcon, BellOffIcon, BookPlusIcon, ChevronsUpDownIcon, EllipsisIcon, ImageIcon, ListTodoIcon, PaperclipIcon, PenLineIcon, ReplyIcon, SparklesIcon, CodeIcon, FileTextIcon, CheckIcon } from "lucide-react"
import { cn } from "cn"
import { Button } from "@/components/ui/button"
import { Textarea } from "@/components/ui/textarea"
import { Spinner } from "@/components/ui/spinner"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { fileUrl } from "@/api"
import { Page, Empty, Meta, SectionLabel, ItemRef, ItemNo, go } from "@/ui"
import { Unwired } from "@/unwired"
import { refresh, tm } from "@/store"
import { ContactAvatar } from "@/people/avatar"
import { size } from "@/attach"
import { useLoad, useBusy } from "@/accounts/load"
import { mailApi, type Message, type Thread, type MailAttachment, type Addr } from "@/api/mail"
import { ThreadDraft } from "@/mail/draft"
import { useT } from "@/i18n"
import { date } from "@/i18n/format"

const who = (a: Addr) => a.name || a.addr
const full = (d: number) => date(d, { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" })
const reSubject = (s: string) => (/^\s*(re|回复|答复)\s*[:：]/i.test(s) ? s : "Re: " + s)

export function ThreadView({ id }: { id: string }) {
  const t = useT()
  const T = useLoad(() => mailApi.thread(id), [id], ["mail"])
  const read = useRef(false)
  useEffect(() => { if (T.s === "ok" && T.data.unread > 0 && !read.current) { read.current = true; mailApi.read(id).catch(() => {}) } }, [T, id])
  const back: [string, string] = ["#/mail", t("mail:shared.title")]
  if (T.s === "unwired") return <Unwired id="mail" />
  if (T.s === "loading") return <Page title={t("mail:shared.title")} back={back}><div className="flex justify-center py-16"><Spinner className="size-5 text-muted-foreground" /></div></Page>
  if (T.s === "error") return <Page title={t("mail:shared.title")} back={back}><Empty title={T.status === 404 ? t("mail:thread.notFound") : T.err} /></Page>
  return <ThreadPage t={T.data} reload={T.reload} back={back} />
}

function ThreadPage({ t, reload, back }: { t: Thread; reload: () => Promise<void>; back: [string, string] }) {
  const tr = useT()
  const last = t.messages.length - 1
  const [open, setOpen] = useState<Set<string>>(() => new Set(last >= 0 ? [t.messages[last].id] : []))
  useEffect(() => { if (last >= 0) setOpen((o) => new Set([...o, t.messages[last].id])) }, [t.messages[last]?.id])
  const toggle = (mid: string) => setOpen((o) => { const n = new Set(o); if (n.has(mid)) n.delete(mid); else n.add(mid); return n })
  const allOpen = t.messages.every((m) => open.has(m.id))
  const drafts = t.drafts.filter((d) => d.status !== "sent" && d.status !== "discarded")
  return (
    <Page title={t.subject || tr("mail:shared.noSubject")} back={back} max={760} small
      sub={<Meta><span>{t.connection.address}</span><span>{tr("mail:thread.count", { count: t.count })}</span>{t.signal && <span>{t.signal.outcome_label}</span>}{t.state === "muted" && <span>{tr("mail:thread.muted")}</span>}{t.state === "done" && <span>{tr("mail:thread.done")}</span>}{t.item_id && <ItemNo id={t.item_id} />}</Meta>}
      actions={<ThreadActions t={t} reload={reload} />}>
      <div className="flex flex-col gap-6">
        {t.messages.length > 1 && <div className="-mb-4 flex justify-end"><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setOpen(allOpen ? new Set([t.messages[last].id]) : new Set(t.messages.map((m) => m.id)))} data-expand-all>
          <ChevronsUpDownIcon />{allOpen ? tr("mail:thread.collapse") : tr("mail:thread.expandAll")}</Button></div>}
        <ol className="flex flex-col overflow-hidden rounded-xl bg-card shadow-[var(--lift)]" aria-label={tr("mail:thread.messagesAria")} data-messages>
          {t.messages.map((m, n) => <li key={m.id} className={cn(n > 0 && "border-t")}><MessageBlock m={m} open={open.has(m.id)} onToggle={() => toggle(m.id)} /></li>)}
        </ol>
        {drafts.map((d) => <ThreadDraft key={d.id} d={d} onChange={() => void reload()} />)}
        <Compose t={t} hasDraft={drafts.length > 0} reload={reload} />
        {t.items.length > 0 && <section><SectionLabel>{tr("mail:thread.related")}</SectionLabel>{t.items.map((i) => <ItemRef key={i.id} id={i.id} />)}</section>}
      </div>
    </Page>
  )
}

/* ───────── one message ───────── */
function MessageBlock({ m, open, onToggle }: { m: Message; open: boolean; onToggle: () => void }) {
  const t = useT()
  const [quoted, setQuoted] = useState(false), [html, setHtml] = useState(false)
  const sep = t("mail:shared.listSep")
  const line = (m.text_new ?? m.text ?? "").replace(/\s+/g, " ").trim()
  const head = (
    <button type="button" onClick={onToggle} aria-expanded={open} className="flex w-full items-start gap-3 px-4 py-3.5 text-left hover:bg-accent/60" data-message-toggle={m.id}>
      <ContactAvatar name={who(m.from)} size={28} className="mt-0.5" />
      <span className="min-w-0 flex-1">
        <span className="flex items-baseline gap-2"><span className="min-w-0 truncate type-body font-medium">{m.direction === "out" ? t("mail:thread.me") : who(m.from)}</span>
          {open && m.direction === "in" && m.from.name && <span className="hidden min-w-0 truncate type-footnote text-muted-foreground sm:inline">{m.from.addr}</span>}
          <span className="flex-1" />{m.attachments.length > 0 && <PaperclipIcon className="size-3.5 shrink-0 text-muted-foreground" aria-label={t("mail:shared.hasAttachments")} />}
          <span className="shrink-0 type-footnote tabular-nums text-muted-foreground" title={full(m.date)}>{tm(m.date)}</span></span>
        {open ? <span className="block truncate type-footnote text-muted-foreground">{m.cc.length ? t("mail:thread.toCc", { to: m.to.map(who).join(sep) || "—", cc: m.cc.map(who).join(sep) }) : t("mail:thread.to", { to: m.to.map(who).join(sep) || "—" })}</span>
          : <span className="block truncate type-callout text-muted-foreground" data-message-line>{line || (m.purged ? t("mail:thread.purged") : t("mail:thread.noBody"))}</span>}
      </span>
    </button>
  )
  if (!open) return <div data-message={m.id} data-open="0">{head}</div>
  const hasQuote = m.text != null && m.text_new != null && m.text.trim() !== m.text_new.trim()
  return (
    <div data-message={m.id} data-open="1">
      {head}
      <div className="px-4 pb-5 anim-in sm:pl-14">
        {m.purged ? <div className="rounded-lg bg-muted/60 px-3 py-2 type-callout text-muted-foreground">{t("mail:thread.purgedSummary", { line: line || "—" })}</div>
          : html ? <HtmlBody id={m.id} />
          : <div className="whitespace-pre-wrap break-words type-body" data-message-text>{quoted ? m.text : m.text_new ?? m.text ?? ""}</div>}
        <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 type-footnote text-muted-foreground">
          {!html && hasQuote && <button type="button" className="hover:text-foreground" onClick={() => setQuoted(!quoted)} aria-expanded={quoted} data-show-quote>{quoted ? t("mail:thread.hideQuote") : t("mail:thread.showQuote")}</button>}
          {m.has_html && !m.purged && <button type="button" className="inline-flex items-center gap-1 hover:text-foreground" onClick={() => setHtml(!html)} aria-pressed={html} data-show-html>{html ? <><FileTextIcon className="size-3.5" aria-hidden />{t("mail:thread.showPlain")}</> : <><CodeIcon className="size-3.5" aria-hidden />{t("mail:thread.showHtml")}</>}</button>}
        </div>
        {m.attachments.length > 0 && <div className="mt-3 flex flex-wrap gap-1.5">{m.attachments.map((a) => <AttachmentChip key={a.id} a={a} />)}</div>}
      </div>
    </div>
  )
}

/** the original HTML: an iframe with an empty sandbox (no scripts, forms, popups or same-origin); the Hub's CSP blocks
 *  remote images until you ask for them */
function HtmlBody({ id }: { id: string }) {
  const t = useT()
  const [images, setImages] = useState(false)
  return (
    <div className="flex flex-col gap-1.5" data-html-body={images ? "images" : "blocked"}>
      {!images && <div className="flex items-center gap-2 rounded-lg bg-muted/60 px-3 py-1.5 type-footnote text-muted-foreground"><ImageIcon className="size-3.5" aria-hidden />{t("mail:thread.imagesBlocked")}
        <button type="button" className="ml-auto shrink-0 whitespace-nowrap text-blue hover:underline" onClick={() => setImages(true)} data-show-images>{t("mail:thread.showImages")}</button></div>}
      <iframe key={String(images)} title={t("mail:thread.frameTitle")} sandbox="" referrerPolicy="no-referrer" loading="lazy" src={mailApi.htmlUrl(id, images)}
        className="h-[480px] w-full resize-y rounded-lg border bg-white" data-html-frame />
    </div>
  )
}

function AttachmentChip({ a }: { a: MailAttachment }) {
  const [busy, act] = useBusy(), [fid, setFid] = useState(a.file_id)
  const open = async () => {
    let f = fid
    if (!f) { const r = await act(() => mailApi.landAttachment(a.id)); if (!r) return; f = r.id; setFid(f) }
    window.open(fileUrl(f!, true), "_blank", "noopener")
  }
  return (
    <button type="button" disabled={busy} onClick={() => void open()} className="inline-flex h-8 max-w-[260px] items-center gap-1.5 rounded-full bg-muted px-3 type-footnote hover:bg-accent disabled:opacity-60" data-mail-attachment={a.id}>
      {busy ? <Spinner className="size-3.5" /> : <PaperclipIcon className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />}<span className="min-w-0 truncate">{a.name}</span><span className="shrink-0 tabular-nums text-muted-foreground">{size(a.size)}</span>
    </button>
  )
}

/* ───────── actions ───────── */
function ThreadActions({ t, reload }: { t: Thread; reload: () => Promise<void> }) {
  const tr = useT()
  const [busy, act] = useBusy()
  const done = async (p: Promise<unknown> | undefined) => { if (await p) { await reload(); await refresh() } }
  return (
    <DropdownMenu>
      <DropdownMenuTrigger render={<Button size="sm" variant="ghost" aria-label={tr("mail:thread.moreActions")} disabled={busy} data-thread-more />}><EllipsisIcon /></DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-48">
        {!t.item_id && <DropdownMenuItem onClick={() => void act(() => mailApi.toItem(t.id), tr("mail:thread.toItemDone")).then((r) => r && go("#/items/" + r.item_id))} data-thread-to-item><ListTodoIcon />{tr("mail:thread.toItem")}</DropdownMenuItem>}
        <DropdownMenuItem onClick={() => void act(() => mailApi.toKnowledge(t.id), tr("mail:thread.toKbDone"))} data-thread-to-kb><BookPlusIcon />{tr("mail:thread.toKb")}</DropdownMenuItem>
        <DropdownMenuSeparator />
        {t.state !== "done" && <DropdownMenuItem onClick={() => void done(act(() => mailApi.patchThread(t.id, "done"), tr("mail:thread.markDoneDone")))} data-thread-done><CheckIcon />{tr("mail:thread.markDone")}</DropdownMenuItem>}
        <DropdownMenuItem onClick={() => void act(() => mailApi.archive(t.id), tr("mail:thread.archived")).then((r) => r && go("#/mail"))} data-thread-archive><ArchiveIcon />{tr("mail:thread.archive")}</DropdownMenuItem>
        <DropdownMenuItem onClick={() => void done(act(() => mailApi.patchThread(t.id, t.state === "muted" ? "open" : "muted"), t.state === "muted" ? tr("mail:thread.unmuted") : tr("mail:thread.mutedToast")))} data-thread-mute><BellOffIcon />{t.state === "muted" ? tr("mail:thread.unmute") : tr("mail:thread.mute")}</DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

/** 让邮件 Bot 起草 (with what you want said) or 自己回复 (an empty draft to the last sender) */
function Compose({ t, hasDraft, reload }: { t: Thread; hasDraft: boolean; reload: () => Promise<void> }) {
  const tr = useT()
  const [mode, setMode] = useState<"" | "bot">(""), [ins, setIns] = useState(""), [busy, act] = useBusy()
  const lastIn = [...t.messages].reverse().find((m) => m.direction === "in")
  const botDraft = async () => { const r = await act(() => mailApi.quickDraft(t.id, { instruction: ins.trim() || undefined }), tr("mail:thread.botReady")); if (r) { setMode(""); setIns(""); await reload(); await refresh() } }
  const mine = async () => {
    if (!lastIn) return
    const r = await act(() => mailApi.newDraft({ connection_id: t.connection.id, mode: "reply", thread_id: t.id, reply_to: lastIn.id, to: [{ addr: lastIn.from.addr, name: lastIn.from.name }], subject: reSubject(t.subject), body: "" }))
    if (r) await reload()
  }
  if (hasDraft && !mode) return null
  if (mode === "bot") return (
    <form className="flex flex-col gap-3 rounded-xl bg-card p-4 shadow-[var(--lift)] anim-in" onSubmit={(e) => { e.preventDefault(); void botDraft() }} data-bot-draft-form>
      <label htmlFor="mail-ins" className="type-callout text-muted-foreground">{tr("mail:thread.insLabel")}</label>
      <Textarea id="mail-ins" autoFocus className="min-h-16 rounded-lg" maxLength={2000} placeholder={tr("mail:thread.insPlaceholder")} value={ins} onChange={(e) => setIns(e.target.value)} data-bot-draft-ins />
      <div className="flex justify-end gap-2"><Button type="button" variant="ghost" onClick={() => setMode("")}>{tr("common:action.cancel")}</Button><Button type="submit" disabled={busy} data-bot-draft-go>{busy ? <Spinner /> : <SparklesIcon />}{busy ? tr("mail:thread.drafting") : tr("mail:thread.draft")}</Button></div>
    </form>
  )
  return (
    <div className="flex flex-wrap gap-2" data-compose>
      <Button variant="outline" onClick={() => setMode("bot")} data-bot-draft><PenLineIcon />{tr("mail:thread.botDraft")}</Button>
      {lastIn && <Button variant="outline" disabled={busy} onClick={() => void mine()} data-reply-mine><ReplyIcon />{tr("mail:thread.replyMine")}</Button>}
    </div>
  )
}
