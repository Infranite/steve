/* The harness's own session behind a Steve session (ST-28): its id, the command that resumes it in the harness,
 * the working directory, and whether the harness's transcript of that id is on the Hub's machine. */
import { api, type RResume } from "@/api"
import { Button } from "@/components/ui/button"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { st, tm, toast, type Sess } from "@/store"
import { CircleAlertIcon, CircleCheckIcon, CopyIcon, SquareTerminalIcon } from "lucide-react"
import { cn } from "cn"
import { useEffect, useState, type ReactNode } from "react"

export const homeTilde = (p: string) => p.replace(/^\/Users\/[^/]+|^\/home\/[^/]+/, "~")
const copy = (text: string, what: string) => { void navigator.clipboard?.writeText(text).then(() => toast(`已复制${what}`), () => toast("浏览器不允许写剪贴板")) }

/** why a session has no harness id yet */
export const noIdNote = (s: Sess) => s.st === "run" ? "harness 还在启动，返回会话 ID 后显示在这里" : "还没有拿到：这个会话没有成功启动过 harness"

function CopyButton({ text, what, testId }: { text: string; what: string; testId?: string }) {
  return <Tooltip><TooltipTrigger render={<Button variant="ghost" size="icon-xs" aria-label={"复制" + what} className="shrink-0 text-muted-foreground" onClick={() => copy(text, what)} data-copy={testId} />}><CopyIcon /></TooltipTrigger><TooltipContent>复制{what}</TooltipContent></Tooltip>
}

/** one line for lists: the id's head, a copy button for the whole id, and a popover with the resume details */
export function NativeIdLine({ k, label, className }: { k: string; label?: string; className?: string }) {
  const s = st.S[k]
  if (!s) return null
  const name = label ?? s.h ?? s.harness
  if (!s.nativeId) return <span className={cn("type-footnote text-tertiary", className)} data-native-id="none">{label ?? `${name} · 会话 ID`} 待获取</span>
  return (
    <span className={cn("flex min-w-0 items-center gap-1 type-footnote text-muted-foreground", className)} data-native-id={s.nativeId}>
      <span className="shrink-0">{name}</span>
      <button type="button" title={s.nativeId} className="min-w-0 truncate font-mono text-[11.5px] hover:text-foreground" onClick={() => copy(s.nativeId!, "会话 ID")}>{s.nativeId.slice(0, 8)}</button>
      <CopyButton text={s.nativeId} what="会话 ID" testId="native-id" />
      <Popover>
        <PopoverTrigger render={<Button variant="ghost" size="xs" className="-ml-1 h-5 px-1.5 text-muted-foreground" data-resume-open={k} />}><SquareTerminalIcon />恢复</PopoverTrigger>
        <PopoverContent align="end" className="w-[360px] max-w-[calc(100vw-24px)]"><ResumeCard k={k} /></PopoverContent>
      </Popover>
    </span>
  )
}

function Field({ label, value, mono = true, what, testId, children }: { label: string; value: string; mono?: boolean; what: string; testId: string; children?: ReactNode }) {
  return (
    <div className="flex flex-col gap-1" data-resume-field={testId}>
      <div className="flex items-center justify-between gap-2 type-footnote text-muted-foreground"><span>{label}</span><CopyButton text={value} what={what} testId={testId} /></div>
      <div className={cn("break-all rounded-lg bg-muted px-2.5 py-1.5 text-foreground", mono ? "font-mono text-[12px] leading-5" : "type-callout")}>{value}</div>
      {children}
    </div>
  )
}

/** what the transcript check found, in one sentence */
function Check({ r, h }: { r: RResume; h: string }) {
  const t = r.transcript
  if (!t) return <p className="flex gap-1.5 type-footnote text-attention" data-resume-check="missing"><CircleAlertIcon className="mt-0.5 size-3.5 shrink-0" />Hub 所在机器上没有找到 {h} 的这份会话记录，可能已被清理或在另一个配置目录，恢复可能失败。</p>
  if (!t.matches) return <p className="flex gap-1.5 type-footnote text-attention" data-resume-check="mismatch"><CircleAlertIcon className="mt-0.5 size-3.5 shrink-0" />找到了 {h} 的记录，但它记的{t.id && t.id !== r.id ? ` ID 是 ${t.id}` : ` 目录是 ${homeTilde(t.cwd ?? "")}`}，和这个会话不一致。</p>
  return <p className="flex gap-1.5 type-footnote text-muted-foreground" data-resume-check="ok"><CircleCheckIcon className="mt-0.5 size-3.5 shrink-0 text-success" />已核对：{h} 的会话记录在本机{t.updated ? `，最后更新 ${tm(t.updated)}` : ""}，ID 和目录与这个会话一致。</p>
}

/** the full resume details; fetched when shown, again when the session's id or state changes */
export function ResumeCard({ k }: { k: string }) {
  const s = st.S[k]
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
  if (err && !r) return <p className="type-footnote text-muted-foreground" data-resume="error">读不到恢复信息：{err}</p>
  if (!r) return <p className="type-footnote text-muted-foreground" data-resume="loading">正在读取…</p>
  return (
    <div className="flex flex-col gap-3" data-resume={r.id ? "ready" : "none"}>
      <div className="flex items-center gap-2 type-callout"><SquareTerminalIcon className="size-4 text-muted-foreground" /><span className="font-medium">{h}</span><span className="type-footnote text-muted-foreground">原始会话</span></div>
      {r.id ? <Field label="原始 Session ID" value={r.id} what="会话 ID" testId="id" /> : <p className="rounded-lg bg-muted px-2.5 py-1.5 type-footnote text-muted-foreground" data-resume-pending>Session ID {noIdNote(s)}。</p>}
      {r.id && (r.command
        ? <Field label="恢复命令" value={r.command} what="恢复命令" testId="command" />
        : <p className="type-footnote text-muted-foreground" data-resume-no-command>{h} 没有已知的恢复命令，可以用上面的 ID 在它自己的界面里找。</p>)}
      {r.cwd && <Field label="工作目录" value={r.cwd} what="工作目录" testId="cwd">{!r.cwd_exists && <span className="type-footnote text-attention" data-resume-cwd-missing>这个目录已经不在了，恢复前需要先恢复目录。</span>}</Field>}
      {r.id && <Check r={r} h={h} />}
      {r.id && <ul className="flex list-disc flex-col gap-1 pl-4 type-footnote text-muted-foreground">
        {s.st === "run" && <li data-resume-running>Steve 正在用这个会话：在终端里同时恢复，两边会写同一份记录，最好等它停下{r.harness === "claude-code" ? "，或在命令后加 --fork-session 另开分支" : ""}。</li>}
        <li>在终端里继续的内容不会出现在 Steve 的会话记录里，也没有 Steve 的工具和审批。</li>
      </ul>}
    </div>
  )
}
