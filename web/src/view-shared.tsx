import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import { Field, FieldContent, FieldDescription, FieldLabel, FieldTitle } from "@/components/ui/field"
import { Progress } from "@/components/ui/progress"
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group"
import { SelectContent, SelectItem, SelectTrigger, SelectValue, Select as UISelect } from "@/components/ui/select"
import { botName, closedSt, closeRel, depsOpen, isKind, kindOf, st, tag, tagLabel, WACT, waitLine, type Item } from "@/store"
import { itemDisplayStatus, workflowRelation, workflowRelationText } from "@/item-workflow-relation"
import { BotAvatar, go, ItemNo, Meta, RelNote, StatusDot } from "@/ui"
import { cn } from "cn"
import { FolderIcon, GitBranchIcon, StarIcon } from "lucide-react"
import { type ReactNode } from "react"
import { t as tr, useT } from "@/i18n"

/* the due presets the web writes are stored as these Chinese values; only their labels are translated */
const DUE_LABEL = { 无: "views:item.dueNone", "今天 18:00": "views:item.dueToday", 明天: "views:item.dueTomorrow", 周五: "views:item.dueFriday", 下周一: "views:item.dueNextMonday" } as const
/** the label of a stored due value, or the value itself when it is not a preset (a date the Hub wrote, say) */
export const dueLabel = (v: string): string => (v in DUE_LABEL ? String(tr(DUE_LABEL[v as keyof typeof DUE_LABEL])) : v)

/** the item's project as a link; items without one say so, since the work then happens in a scratch directory */
export function ProjectChip({ id, className, plain }: { id: string | null; className?: string; plain?: boolean }) {
  const p = st.projects.find((x) => x.id === id), t = useT()
  if (!p) return <span className={cn("inline-flex items-center gap-1", className)} data-project-chip="none"><FolderIcon className="size-3.5" strokeWidth={1.75} />{t("shell:item.noProject")}</span>
  /* plain: inside a clickable row or card, where a nested link would be a control inside a control */
  if (plain) return <span className={cn("inline-flex min-w-0 items-center gap-1", className)} data-project-chip={p.id}><FolderIcon className="size-3.5 shrink-0" strokeWidth={1.75} /><span className="truncate">{p.name}</span></span>
  return <a href={"#/projects/" + p.id} onClick={(e) => e.stopPropagation()} className={cn("inline-flex items-center gap-1 text-foreground/80 hover:text-foreground hover:underline", className)} data-project-chip={p.id}><FolderIcon className="size-3.5" strokeWidth={1.75} />{p.name}</a>
}

export function Select({ value, options, onChange, align = "start", testId, quiet, className }: { value: string; options: { v: string; l: ReactNode; disabled?: boolean }[]; onChange: (v: string) => void; align?: "start" | "end"; testId?: string; quiet?: boolean; className?: string }) {
  return (
    <UISelect items={Object.fromEntries(options.map((o) => [o.v, o.l]))} value={value} onValueChange={(v) => v != null && onChange(String(v))}>
      <SelectTrigger size="sm" className={cn("min-w-0 max-w-full rounded-lg bg-transparent font-normal", quiet ? "-ml-px border-transparent px-2 hover:bg-accent [&>svg]:opacity-0 hover:[&>svg]:opacity-100" : "border-border", className)} data-select={testId}><SelectValue className="min-w-0 overflow-hidden" /></SelectTrigger>
      <SelectContent align={align === "end" ? "end" : "start"} className="min-w-44">{options.map((o) => <SelectItem key={o.v} value={o.v} disabled={o.disabled}>{o.l}</SelectItem>)}</SelectContent>
    </UISelect>
  )
}

export const selected = new Set<string>();

/** the count after a section heading's label, one step quieter than the label */
export const Count = ({ n }: { n: number }) => (n ? <span className="ml-2 font-normal tabular-nums text-tertiary/80">{n}</span> : null)

/** plan progress as a count with a short bar; on a finished item only when steps were left open, which is worth a look */
export function PlanBar({ i, warn }: { i: Item; warn?: boolean }) {
  const t = useT()
  if (!i.plan?.length) return null
  const dn = i.plan.filter((x) => x[1] === 1).length
  return <span className={cn("inline-flex items-center gap-1.5 tabular-nums", warn && "text-attention")} title={t("shell:item.plan", { done: dn, total: i.plan.length })} data-plan>{dn}/{i.plan.length}<Progress value={(dn / i.plan.length) * 100} className={cn("hidden w-8 sm:flex [&_[data-slot=progress-track]]:h-1", warn ? "[&_[data-slot=progress-indicator]]:bg-attention" : "[&_[data-slot=progress-indicator]]:bg-foreground/60")} /></span>
}

/** one item in a list. Open items take two lines: the title with what tells it apart on the right (tags, due, plan,
 *  bot, time), then what it is doing now. Finished and dropped items fold into one line, their outcome after the title.
 *  `proj` / `bot` off: the list is already about one project or bot, so the row doesn't repeat it */
export function ItemRow({ i, pick, proj = true, bot = true }: { i: Item; pick?: () => void; proj?: boolean; bot?: boolean }) {
  const t = useT()
  const on = selected.has(i.id), closed = closedSt(i.st), w = i.wait
  const relation = workflowRelation(i, st.items)
  const planOpen = !!i.plan?.length && i.plan.some((x) => x[1] !== 1)
  const extra = [
    i.ss.length > 1 && <span key="ss" className="shrink-0 whitespace-nowrap">{t("shell:item.sessions", { count: i.ss.length })}</span>,
    i.q.length > 0 && <span key="q" className="shrink-0 whitespace-nowrap">{t("shell:item.queued", { count: i.q.length })}</span>,
    i.parent && <span key="p" className="inline-flex shrink-0 items-center gap-1 whitespace-nowrap"><GitBranchIcon className="size-3" />{t("shell:item.forked")}</span>,
    !!i.deps?.length && <span key="d" className="shrink-0 whitespace-nowrap">{t("shell:item.deps", { done: i.deps.length - depsOpen(i).length, total: i.deps.length })}</span>,
  ].filter(Boolean)
  /* a list row carries one tag, its kind, as quiet text (colour stays for status); the rest are in its hover title and
   * on the item page */
  const kind = kindOf(i) ? tag(kindOf(i)!) : undefined
  const others = (i.tags ?? []).map(tag).filter((x) => x && !isKind(x)).map((x) => tagLabel(x!))
  const side = (
    <span className="ml-auto flex shrink-0 items-center gap-2.5 pl-2 type-footnote text-muted-foreground">
      {kind && <span className="max-w-24 truncate text-tertiary" title={[kind.name, ...others].join(" · ")} data-kind={kind.name}>{kind.name}</span>}
      {i.pri === "高" && !closed && <span className="inline-flex h-5 items-center rounded-md bg-attention/12 px-1.5 font-medium text-attention">{t("shell:item.high")}</span>}
      {i.due && !closed && <span className={cn("whitespace-nowrap", /今天/.test(i.due) && "text-foreground/80")}>{t("shell:item.due", { due: dueLabel(i.due) })}</span>}
      {proj && i.project && <span className="hidden max-w-28 md:inline-flex"><ProjectChip id={i.project} plain /></span>}
      {(!closed || planOpen) && <PlanBar i={i} warn={closed} />}
      {bot && i.b && <span title={botName(i.b)} className="hidden sm:inline-flex"><BotAvatar k={i.b} size={18} /></span>}
      <span className="min-w-11 whitespace-nowrap text-right tabular-nums text-tertiary">{i.up}</span>
    </span>
  )
  return (
    <div role="button" tabIndex={0} onClick={() => (pick ? pick() : go("#/items/" + i.id))} onKeyDown={(e) => e.key === "Enter" && go("#/items/" + i.id)} data-item-row={i.id} data-item-status={itemDisplayStatus(i)} data-dense={closed || undefined}
      className={cn("group flex cursor-pointer gap-2.5 rounded-lg px-2 transition-colors duration-150 hover:bg-accent", closed ? "items-center py-1.5" : "items-start py-2.5", pick && on && "bg-blue/10 hover:bg-blue/10")}>
      {pick ? <Checkbox checked={on} className={cn(!closed && "mt-[3px]")} aria-label={t("shell:item.select", { id: i.id })} data-pick={i.id} /> : <StatusDot s={itemDisplayStatus(i)} className={cn(!closed && "mt-[7px]")} />}
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 items-center gap-2">
          <ItemNo id={i.id} className="w-12" />
          <span className={cn("truncate type-body", closed ? "text-foreground/85" : "font-medium")}>{i.t}</span>
          {i.star && <StarIcon className="size-3.5 shrink-0 fill-current text-tertiary" />}
          {closed && i.now && <span className="hidden min-w-0 shrink-[2] truncate type-footnote text-muted-foreground sm:block" data-item-now>{i.now}</span>}
          {side}
        </div>
        {/* a card waiting on you says what it asks and takes you to it; otherwise the row says what is going on now */}
        {!closed && w && (
          <div className="mt-0.5 flex min-w-0 items-center gap-2 pl-14">
            <Meta className="min-w-0 flex-1 flex-nowrap overflow-hidden [&>*:first-child]:min-w-0 [&>*:first-child]:truncate"><span className="type-callout text-foreground/80" data-item-wait={w.type}>{waitLine(w)}{i.waits.length > 1 && <span className="text-muted-foreground">{t("shell:item.andMore", { count: i.waits.length })}</span>}</span>{extra}</Meta>
            {!pick && <Button size="xs" variant="outline" className="shrink-0" onClick={(e) => { e.stopPropagation(); go("#/items/" + i.id) }} data-item-wait-go={i.id}>{t("shell:item.goAct", { act: WACT[w.type] ?? t("shell:item.handle") })}</Button>}
          </div>
        )}
        {!closed && !w && (i.now || relation || extra.length > 0) && (
          <Meta className="mt-0.5 flex-nowrap pl-14">
            {/* beside a "now" line the relation keeps its width and the line truncates; alone it yields to the counts after it */}
            {relation && <span className={cn("min-w-0 max-w-[60%] truncate type-callout font-medium text-foreground/80", i.now && relation.kind !== "paused" && relation.kind !== "previous" && "shrink-0")} data-workflow-relation={relation.kind}>{workflowRelationText(relation)}</span>}
            {i.now && relation?.kind !== "paused" && relation?.kind !== "previous" && <span className="min-w-0 truncate type-callout" data-item-now>{i.now}</span>}
            {extra}
          </Meta>
        )}
        {i.rel && i.rel.tone !== "ok" && <RelNote rel={i.rel} className="mt-1 pl-14" onClose={i.rel.tone === "fail" ? () => closeRel(i) : undefined} />}
      </div>
    </div>
  )
}

export const ItemRowExport = ItemRow


/* ═════════ dialogs ═════════ */
/* shadcn “choice card”: RadioGroup + FieldLabel wrapping a horizontal Field */
export function ChoiceCards<T extends string>({ value, onChange, options, cols = 2 }: { value: T; onChange: (v: T) => void; options: { v: T; t: string; d?: string; test?: string }[]; cols?: number }) {
  return (
    <RadioGroup value={value} onValueChange={(v) => onChange(v as T)} className={cn("gap-2", cols === 2 ? "grid-cols-2" : "grid-cols-1")}>
      {options.map((o) => (
        <FieldLabel key={o.v} htmlFor={"cc-" + o.v} className="rounded-xl! has-data-checked:border-blue! has-data-checked:bg-blue/5!" data-mode={o.test ?? o.v}>
          <Field orientation="horizontal" className="p-3.5!">
            <FieldContent><FieldTitle className="type-callout font-medium">{o.t}</FieldTitle>{o.d && <FieldDescription className="type-footnote">{o.d}</FieldDescription>}</FieldContent>
            <RadioGroupItem value={o.v} id={"cc-" + o.v} />
          </Field>
        </FieldLabel>
      ))}
    </RadioGroup>
  )
}
