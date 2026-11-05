/* 对话式配置的界面：每个配置页顶部的“告诉 Steve”，以及改动草稿卡（也用在主对话里）。
 * Steve drafts a field-level change on the Hub; you apply it there, and it can be undone. */
import { useState } from "react"
import { cn } from "cn"
import { SparklesIcon, ArrowRightIcon, CheckIcon, ArrowUpIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Card } from "@/components/ui/card"
import { Spinner } from "@/components/ui/spinner"
import { proposalOf, toast } from "@/store"
import { api } from "@/api"
import { go } from "@/ui"
import { useT } from "@/i18n"

export type Ctx = { kind: "bot" | "project" | "settings" | "models" | "tags" | "global"; id?: string }

export function ProposalCard({ p, inChat = false }: { p: any; inChat?: boolean }) {
  const [busy, setBusy] = useState(false), t = useT()
  const done = p.state === "applied", dropped = p.state === "dropped"
  const run = async (f: () => Promise<void>) => { setBusy(true); try { await f() } finally { setBusy(false) } }
  return (
    <Card size="sm" className={cn("gap-0 py-0", inChat ? "w-[min(520px,88vw)]" : "", dropped && "opacity-60")} data-proposal={p.state}>
      <div className="flex items-center gap-2 px-4 pb-1.5 pt-3 type-footnote text-muted-foreground"><SparklesIcon className="size-3.5" />{t("shell:config.drafted", { title: p.title })}
        {inChat && <button type="button" onClick={() => go(p.href)} className="ml-auto hover:text-foreground">{t("shell:config.openSettings")}</button>}</div>
      <div className="flex flex-col px-4 pb-3">{p.changes.map((c: any, n: number) => (
        <div key={n} className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5 border-t py-2 first:border-t-0 type-callout" data-change={c.label}>
          <span className="w-full type-footnote text-muted-foreground sm:w-40 sm:shrink-0">{c.label}</span>
          <span className="text-muted-foreground line-through decoration-tertiary">{c.from}</span><ArrowRightIcon className="size-3 shrink-0 self-center text-tertiary" /><span className="text-foreground">{c.to}</span>
        </div>))}</div>
      <div className="flex items-center justify-end gap-2 border-t px-3 py-2.5">
        {done ? <><span className="mr-auto inline-flex items-center gap-1.5 type-footnote text-muted-foreground"><CheckIcon className="size-3.5 text-success" />{t("shell:config.applied")}</span><Button variant="ghost" disabled={busy} onClick={() => run(p.undo)} data-undo>{t("shell:config.undo")}</Button></>
          : dropped ? <span className="mr-auto type-footnote text-muted-foreground">{t("shell:config.dropped")}</span>
          : <><Button variant="ghost" disabled={busy} onClick={() => run(p.drop)} data-drop>{t("shell:config.drop")}</Button><Button disabled={busy} onClick={() => run(p.apply)} data-apply>{busy && <Spinner className="size-3.5" />}{t("shell:config.apply")}</Button></>}
      </div>
    </Card>
  )
}

/** the “just tell Steve” bar at the top of a config page */
export function AskSteve({ ctx, placeholder, className }: { ctx: Ctx; placeholder: string; className?: string }) {
  const [v, setV] = useState(""), [busy, setBusy] = useState(false), [p, setP] = useState<any>(null), [note, setNote] = useState(""), t = useT()
  const send = async () => {
    const text = v.trim(); if (!text || busy) return
    setBusy(true); setP(null); setNote("")
    try { const r = await api.draft(ctx, text); if (r.changes.length) { setP(proposalOf(r)); setV("") } else setNote(t("shell:config.noChange", { title: r.title })) }
    catch (e) { toast((e as Error).message) } finally { setBusy(false) }
  }
  return (
    <div className={cn("flex flex-col gap-3", className)} data-ask-steve>
      <div className="flex items-center gap-2 rounded-2xl border bg-card py-1.5 pl-3.5 pr-1.5 shadow-[0_1px_3px_rgb(0_0_0/0.04)] focus-within:border-blue/60">
        <SparklesIcon className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.75} />
        <input value={v} onChange={(e) => setV(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && !e.nativeEvent.isComposing) void send() }} placeholder={placeholder}
          className="h-8 min-w-0 flex-1 bg-transparent text-[14px] outline-none placeholder:text-tertiary" data-ask-input />
        <Button size="icon-sm" className="rounded-full" disabled={!v.trim() || busy} onClick={() => void send()} aria-label={t("shell:config.askDraft")} data-ask-send>{busy ? <Spinner className="size-3.5" /> : <ArrowUpIcon className="size-4" />}</Button>
      </div>
      {busy && <div className="px-1 type-footnote text-muted-foreground shimmer">{t("shell:config.drafting")}</div>}
      {note && <div className="px-1 type-footnote text-muted-foreground">{note}</div>}
      {p && <ProposalCard p={p} />}
    </div>
  )
}
