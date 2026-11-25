/* 回复依据 (channels-personas.md §2.6, prototype channels.tsx Basis): which knowledge a reply used, at what level, and how
 * it went out. Only you see it; the people in the chat only see the reply. */
import { ShieldCheckIcon, EyeOffIcon } from "lucide-react"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { StatusDot } from "@/ui"
import { LEVEL, OUT_LABEL, type Basis as B } from "@/api/channels"
import { useT } from "@/i18n"

export function BasisView({ b }: { b: B | null }) {
  const t = useT()
  /* a check that did not finish (or still runs) is said even without knowledge: it is not a pass, and not a leak */
  const pending = b?.verdict === "unchecked" || b?.verdict === "checking"
  if (!b || (!b.entries.length && !b.rewrites.length && !pending)) return null
  const state = b.verdict === "drafted" ? t("channels:basis.drafted") : b.verdict === "unchecked" ? t("channels:basis.unchecked") : b.verdict === "checking" ? t("channels:basis.checking") : ""
  const summary = [t("channels:basis.title"), t("channels:basis.entries", { count: b.entries.length }), b.rewrites.length ? t("channels:basis.rewrites", { count: b.rewrites.length }) : "", state].filter(Boolean).join(" · ")
  return (
    <Collapsible>
      <CollapsibleTrigger className="mt-1 inline-flex items-center gap-1 rounded-md type-footnote text-muted-foreground hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring" data-basis>
        <ShieldCheckIcon className="size-3.5" aria-hidden />{summary}</CollapsibleTrigger>
      <CollapsibleContent data-basis-body><div className="pt-2"><div className="flex flex-col gap-1.5 rounded-lg bg-background/70 px-3 py-2.5">
        {b.entries.map((x, n) => { const [out, dot] = OUT_LABEL[x.out] ?? [x.out, "idle"]; return (
          <div key={n} className="flex items-start gap-2 type-footnote"><StatusDot s={dot} className="mt-1" />
            <div className="min-w-0 flex-1"><div className="break-words text-foreground">{x.used}</div><div className="text-muted-foreground">{x.source_label} · {x.effective !== x.level ? t("channels:basis.levelChanged", { level: LEVEL[x.effective]?.[0] ?? x.effective, from: LEVEL[x.level]?.[0] ?? x.level }) : LEVEL[x.effective]?.[0] ?? x.effective} · {out}</div></div></div>) })}
        {b.rewrites.map((r, n) => <div key={"r" + n} className="flex items-start gap-2 border-t pt-1.5 type-footnote text-muted-foreground"><EyeOffIcon className="mt-0.5 size-3.5 shrink-0" aria-hidden /><span>{t("channels:basis.rewrite", { from: r.from, to: r.to, reason: r.reason })}</span></div>)}
        <div className="type-footnote text-tertiary">{t("channels:basis.onlyYou")}</div>
      </div></div></CollapsibleContent>
    </Collapsible>
  )
}
