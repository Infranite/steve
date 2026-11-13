/* Knowledge pieces that live on other pages (W15): the "依据" fold under Steve's answers, a session's
 * "用到的知识", and the confirm/mem card in 待处理. Kept free of views.tsx imports: ui.tsx uses this file. */
import { useEffect, useState } from "react"
import { cn } from "cn"
import { BookOpenIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { CardContent, CardFooter } from "@/components/ui/card"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { Textarea } from "@/components/ui/textarea"
import { StatusDot, SectionLabel, go } from "@/ui"
import { kb, levelLabel, LEVEL_DOT, type Cite, type Use } from "@/api/knowledge"
import { NotWired } from "@/api"
import { t, useT } from "@/i18n"

/** `[n]` marks in an answer become links to their snippet; marks without a cite stay text */
export function linkCites(text: string, cites?: Cite[]) {
  if (!cites?.length) return text
  const by = new Map(cites.map((c) => [c.n, c]))
  return text.replace(/\[(\d{1,2})\](?!\()/g, (m, n) => { const c = by.get(Number(n)); return c?.href ? `[\\[${n}\\]](${c.href} "${c.label.replace(/"/g, "'")}")` : m })
}

/** under a main-thread answer: "依据 · n 条知识", folded; each entry opens its snippet */
export function Cites({ cites }: { cites?: Cite[] }) {
  const t = useT()
  if (!cites?.length) return null
  return (
    <Collapsible className="-mt-1 self-start pl-1" data-cites>
      <CollapsibleTrigger className="inline-flex items-center gap-1 rounded-md type-footnote text-muted-foreground hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring" data-cites-toggle>
        <BookOpenIcon className="size-3.5" strokeWidth={1.75} />{t("knowledge:parts.cites", { count: cites.length })}
      </CollapsibleTrigger>
      <CollapsibleContent data-cites-body><div className="pt-2"><div className="flex max-w-[min(520px,88vw)] flex-col gap-0.5 rounded-lg bg-muted/60 px-1.5 py-1.5">
        {cites.map((c) => (
          <button key={c.n} type="button" disabled={!c.href} onClick={() => c.href && go(c.href)} className="flex items-start gap-2 rounded-md px-1.5 py-1 text-left type-footnote enabled:hover:bg-background/70" data-cite={c.n}>
            <StatusDot s={LEVEL_DOT[c.level] ?? "idle"} className="mt-1" />
            <span className="min-w-0 flex-1"><span className="mr-1 font-mono tabular-nums text-muted-foreground">[{c.n}]</span><span className="text-foreground">{c.label}</span>
              <span className="text-muted-foreground"> · {levelLabel(c.level)}</span></span>
          </button>))}
      </div></div></CollapsibleContent>
    </Collapsible>
  )
}

/** session panel: what knowledge the session was given (`GET /kb/uses`): each document once, however many searches
 *  and snippets brought it – its title, where it comes from, its level, how many snippets; why it was looked up (the
 *  searches' queries) on hover. The first three, the rest on request; nothing on a Hub without the module or when the
 *  session used none */
export function SessionKnowledge({ id }: { id: string }) {
  const t = useT()
  const [hits, setHits] = useState<Use[] | null | "absent">(null)
  const [all, setAll] = useState(false)
  useEffect(() => { let on = true; kb.uses("session", id).then((r) => on && setHits(r.hits), (e) => on && setHits(e instanceof NotWired ? "absent" : [])); return () => { on = false } }, [id])
  if (hits === "absent" || hits === null || !hits.length) return null
  const docs = new Map<string, { h: Use; n: number; why: Set<string> }>()
  for (const h of hits) {
    const k = h.type === "chunk" ? h.id : h.type + h.id, d = docs.get(k)
    if (d) { d.n++; d.why.add(h.query) } else docs.set(k, { h, n: 1, why: new Set([h.query]) })
  }
  const list = [...docs.values()], shown = all ? list : list.slice(0, 3)
  return (
    <section data-session-knowledge={list.length}>
      <SectionLabel className="mb-2.5 px-0">{t("knowledge:parts.sessionTitle")}<span className="ml-1.5 tabular-nums">{list.length}</span></SectionLabel>
      <div className="flex flex-col">{shown.map(({ h, n, why }) => (
        <button key={h.use_id + h.n} type="button" disabled={h.deleted || !h.href} onClick={() => go(h.href)} title={t("knowledge:parts.why") + " " + [...why].join(" / ")}
          className="-mx-2 flex w-[calc(100%+1rem)] min-w-0 items-start gap-2.5 rounded-lg px-2 py-1.5 text-left transition-colors duration-120 enabled:hover:bg-accent" data-use={h.n}>
          <StatusDot s={h.deleted ? "ended" : LEVEL_DOT[h.level] ?? "idle"} className="mt-1.5" />
          <span className="min-w-0 flex-1"><span className="block truncate type-callout text-foreground">{h.title || h.label}</span>
            <span className="block truncate type-footnote text-muted-foreground">{h.deleted ? t("knowledge:parts.deleted") : [h.label.split(" · ")[0], levelLabel(h.level), n > 1 ? t("knowledge:parts.snippets", { count: n }) : null].filter(Boolean).join(" · ")}</span></span>
        </button>))}
        {list.length > 3 && <div><button type="button" onClick={() => setAll(!all)} className="mt-1 rounded-md type-footnote text-muted-foreground hover:text-foreground" data-uses-more>{all ? t("views:sessionPanel.less") : t("views:sessionPanel.showAll", { count: list.length })}</button></div>}
      </div>
    </section>
  )
}

/** a candidate's question names what it would become (ST-41): “存为长期记忆吗？…” or “存为知识条目吗？…”;
 *  decisions opened earlier ask “记住这条吗？…”. The Hub words them in the request's language. */
const ASK = /^(存为长期记忆吗？|存为知识条目吗？|记住这条吗？|Save as a long-term memory\? |Save as a knowledge entry\? |Remember this\? )/
export const memAsk = (q: string) => q.match(ASK)?.[1]?.trim() ?? t("knowledge:ask.saveMem")
/** the draft line that says why it is worth keeping, in either language */
const WHY = /^(为什么值得保留：|Why keep it: )/
/** the body of a confirm/mem decision: what it would become and why it is worth keeping, where it came from,
 *  the words it was based on, what it replaces, and the wording you can fix before confirming (sent as the
 *  decision's text) */
export function MemConfirm({ q, draft, busy, act }: { q: string; draft?: string; busy: boolean; act: (c: string, t?: string) => void }) {
  const orig = q.replace(ASK, "")
  const [txt, setTxt] = useState(orig), [edit, setEdit] = useState(false)
  const lines = (draft ?? "").split("\n").filter(Boolean)
  const t = useT()
  const tx = txt.trim()
  return <>
    <CardContent className="flex flex-col gap-2 pb-4" data-mem-confirm>
      {edit ? <Textarea autoFocus aria-label={t("knowledge:parts.wordingAria")} className="min-h-16 rounded-lg" value={txt} onChange={(e) => setTxt(e.target.value)} data-mem-text />
        : <div className="type-body">{tx || orig}</div>}
      {lines.length > 0 && <div className="flex flex-col gap-0.5 type-footnote text-muted-foreground">{lines.map((l, n) => <div key={n} className={cn("break-words", WHY.test(l) && "text-foreground/80")}>{l}</div>)}</div>}
    </CardContent>
    <CardFooter className="flex-wrap justify-end gap-2 border-t px-3 py-2.5!">
      {!edit && <Button variant="ghost" className="mr-auto text-muted-foreground" onClick={() => setEdit(true)} data-mem-edit>{t("knowledge:parts.reword")}</Button>}
      <Button variant="outline" disabled={busy} onClick={() => act("no")} data-mem-no>{t("knowledge:parts.no")}</Button>
      <Button disabled={busy || !tx} onClick={() => act("yes", tx !== orig ? tx : undefined)} data-mem-yes>{t("common:action.save")}</Button>
    </CardFooter>
  </>
}
