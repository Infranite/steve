/* The harness's own session behind a Steve session (ST-28): its id, the command that resumes it in the harness,
 * the working directory, and whether the harness's transcript of that id is on the Hub's machine. */
import { HarnessMark } from "@/components/harness-mark"
import { api, type RResume } from "@/api"
import { Button } from "@/components/ui/button"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { st, tm, toast, type Sess } from "@/store"
import { CircleAlertIcon, CircleCheckIcon, CopyIcon, SquareTerminalIcon } from "lucide-react"
import { cn } from "cn"
import { useEffect, useState, type ReactNode } from "react"
import { t, useT } from "@/i18n"

export const homeTilde = (p: string) => p.replace(/^\/Users\/[^/]+|^\/home\/[^/]+/, "~")
const copy = (text: string, what: string) => { void navigator.clipboard?.writeText(text).then(() => toast(t("shell:resume.copied", { what })), () => toast(t("shell:resume.clipboardBlocked"))) }

/** why a session has no harness id yet */
export const noIdNote = (s: Sess) => s.st === "run" ? t("shell:resume.starting") : t("shell:resume.neverStarted")

function CopyButton({ text, what, testId }: { text: string; what: string; testId?: string }) {
  const t = useT()
  return <Tooltip><TooltipTrigger render={<Button variant="ghost" size="icon-xs" aria-label={t("shell:resume.copy", { what })} className="shrink-0 text-muted-foreground" onClick={() => copy(text, what)} data-copy={testId} />}><CopyIcon /></TooltipTrigger><TooltipContent>{t("shell:resume.copy", { what })}</TooltipContent></Tooltip>
}

/** one line for lists: the id's head, a copy button for the whole id, and a popover with the resume details */
export function NativeIdLine({ k, label, className }: { k: string; label?: string; className?: string }) {
  const s = st.S[k], t = useT()
  if (!s) return null
  const name = label ?? s.h ?? s.harness
  if (!s.nativeId) return <span className={cn("type-footnote text-tertiary", className)} data-native-id="none">{t("shell:resume.pending", { label: label ?? t("shell:resume.idLabel", { name }) })}</span>
  return (
    <span className={cn("flex min-w-0 items-center gap-1 type-footnote text-muted-foreground", className)} data-native-id={s.nativeId}>
      <span className="shrink-0">{name}</span>
      <button type="button" title={s.nativeId} className="min-w-0 truncate font-mono text-foreground/80 hover:text-foreground" onClick={() => copy(s.nativeId!, t("shell:resume.what.id"))}>{s.nativeId.slice(0, 8)}</button>
      <CopyButton text={s.nativeId} what={t("shell:resume.what.id")} testId="native-id" />
      <Popover>
        <PopoverTrigger render={<Button variant="ghost" size="xs" className="-ml-1 h-5 px-1.5 text-muted-foreground" data-resume-open={k} />}><SquareTerminalIcon />{t("shell:resume.resume")}</PopoverTrigger>
        <PopoverContent align="end" className="w-[360px] max-w-[calc(100vw-24px)]"><ResumeCard k={k} /></PopoverContent>
      </Popover>
    </span>
  )
}

function Field({ label, value, mono = true, what, testId, children }: { label: string; value: string; mono?: boolean; what: string; testId: string; children?: ReactNode }) {
  return (
    <div className="flex flex-col gap-1" data-resume-field={testId}>
      <div className="flex items-center justify-between gap-2 type-label text-tertiary"><span>{label}</span><CopyButton text={value} what={what} testId={testId} /></div>
      <div className={cn("break-all rounded-lg bg-muted/60 px-3 py-2 text-foreground", mono ? "type-code" : "type-callout")}>{value}</div>
      {children}
    </div>
  )
}

/* a warning: the icon carries the status colour, the sentence stays readable body text on a faint wash */
const WARN = "flex gap-2 rounded-lg bg-attention/10 px-3 py-2 type-callout text-foreground/80"
/** what the transcript check found, in one sentence */
function Check({ r, h }: { r: RResume; h: string }) {
  const tr = r.transcript, t = useT()
  if (!tr && r.transcript_checked === false) return null
  if (!tr) return <p className={WARN} data-resume-check="missing"><CircleAlertIcon className="mt-0.5 size-4 shrink-0 text-attention" />{t("shell:resume.checkMissing", { h })}</p>
  if (!tr.matches) return <p className={WARN} data-resume-check="mismatch"><CircleAlertIcon className="mt-0.5 size-4 shrink-0 text-attention" />{tr.id && tr.id !== r.id ? t("shell:resume.checkIdMismatch", { h, id: tr.id }) : t("shell:resume.checkCwdMismatch", { h, cwd: homeTilde(tr.cwd ?? "") })}</p>
  return <p className="flex gap-1.5 type-callout text-muted-foreground" data-resume-check="ok"><CircleCheckIcon className="mt-0.5 size-4 shrink-0 text-success" />{tr.updated ? t("shell:resume.checkOkUpdated", { h, time: tm(tr.updated) }) : t("shell:resume.checkOk", { h })}</p>
}

/** the shell command template to fork a session in `harness`, with `{id}` filled in */
const forkCmd = (harness: string, id: string | null): string | null => {
  const tpl = st.runtimes.find((x) => x.id === harness)?.fork_command
  const quoted = id ? (/^[\w./:@%+-][\w./=:@%+,-]*$/.test(id) ? id : `'${id.replace(/'/g, "'\\''")}'`) : ""
  return tpl && id ? tpl.replace("{id}", () => quoted) : null
}

/** the full resume details; fetched when shown, again when the session's id or state changes */
export function ResumeCard({ k }: { k: string }) {
  const s = st.S[k], t = useT()
  const [r, setR] = useState<RResume | null>(null), [err, setErr] = useState("")
  const nid = s?.nativeId, state = s?.st
  useEffect(() => {
    let live = true
    setErr("")
    api.resumeInfo(k).then((x) => { if (live) setR(x) }, (e) => { if (live) setErr(e instanceof Error ? e.message : String(e)) })
    return () => { live = false }
  }, [k, nid, state])
  if (!s) return null
  const h = s.h ?? s.harness ?? "harness"
  if (err && !r) return <p className="type-footnote text-muted-foreground" data-resume="error">{t("shell:resume.loadFailed", { error: err })}</p>
  if (!r) return <p className="type-footnote text-muted-foreground" data-resume="loading">{t("shell:resume.loading")}</p>
  const fork = r.fork_command ?? forkCmd(r.harness, r.id)
  return (
    <div className="flex flex-col gap-3" data-resume={r.id ? "ready" : "none"}>
      <div className="flex items-center gap-2"><HarnessMark harness={s.harness ?? s.h} /><span className="type-headline">{h}</span><span className="type-callout text-muted-foreground">{t("shell:resume.original")}</span></div>
      {r.id ? <Field label={t("shell:resume.originalId")} value={r.id} what={t("shell:resume.what.id")} testId="id" /> : <p className="rounded-lg bg-muted/60 px-3 py-2 type-callout text-muted-foreground" data-resume-pending>{t("shell:resume.idPending", { note: noIdNote(s) })}</p>}
      {r.id && (r.command
        ? <Field label={t("shell:resume.commandLabel")} value={r.command} what={t("shell:resume.what.command")} testId="command" />
        : <p className="type-callout text-muted-foreground" data-resume-no-command>{t("shell:resume.noCommand", { h })}</p>)}
      {r.cwd && <Field label={t("shell:resume.cwdLabel")} value={r.cwd} what={t("shell:resume.what.cwd")} testId="cwd">{!r.cwd_exists && <span className="flex gap-1.5 type-callout text-foreground/80" data-resume-cwd-missing><CircleAlertIcon className="mt-0.5 size-4 shrink-0 text-attention" />{t("shell:resume.cwdMissing")}</span>}</Field>}
      {r.id && <Check r={r} h={h} />}
      {r.id && <ul className="flex list-disc flex-col gap-1 pl-4 type-footnote text-tertiary marker:text-tertiary">
        {s.st === "run" && <li data-resume-running>{fork ? t("shell:resume.runningFork", { fork }) : t("shell:resume.running")}</li>}
        <li>{t("shell:resume.terminalNote")}</li>
      </ul>}
    </div>
  )
}
