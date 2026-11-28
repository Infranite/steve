import { SessionIdentity } from "@/components/session-identity"
import { botName, closedSt, closeRel, depsOpen, getItem, isKind, itemDisplayStatus, kindOf, st, tag, tagLabel, useItemRuntime, WACT, waitLine, type Item } from "@/store"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import { Field, FieldContent, FieldDescription, FieldLabel, FieldTitle } from "@/components/ui/field"
import { Progress } from "@/components/ui/progress"
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group"
import { SelectContent, SelectItem, SelectTrigger, SelectValue, Select as UISelect } from "@/components/ui/select"
import { go, ItemNo, Meta, RelNote, StatusDot } from "@/ui"
import { ActorAvatar } from "@/components/actor-avatar"
import { cn } from "cn"
import { FolderIcon, GitBranchIcon, StarIcon } from "lucide-react"
import { memo, type ReactNode } from "react"
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
  if (plain) return <span title={p.name} className={cn("inline-flex min-w-0 items-center gap-1", className)} data-project-chip={p.id}><FolderIcon className="size-3.5 shrink-0" strokeWidth={1.75} /><span className="truncate">{p.name}</span></span>
  return <a href={"#/projects/" + p.id} onClick={(e) => e.stopPropagation()} className={cn("inline-flex items-center gap-1 text-foreground/80 hover:text-foreground hover:underline", className)} data-project-chip={p.id}><FolderIcon className="size-3.5" strokeWidth={1.75} />{p.name}</a>
}

/** the system's picker. A toolbar filter by default (`sm`, the muted fill every control shares); `size="default"` for a
 *  form field; `quiet` for one that shows as plain text until hovered. `label` names it for assistive tech and tests. */
export function Select({ value, options, onChange, align = "start", testId, quiet, className, label, disabled, placeholder, size = "sm" }: { value: string; options: { v: string; l: ReactNode; disabled?: boolean }[]; onChange: (v: string) => void; align?: "start" | "end"; testId?: string; quiet?: boolean; className?: string; label?: string; disabled?: boolean; placeholder?: string; size?: "sm" | "default" }) {
  return (
    <UISelect items={Object.fromEntries(options.map((o) => [o.v, o.l]))} value={value || (placeholder ? null : value)} onValueChange={(v) => v != null && onChange(String(v))} disabled={disabled}>
      <SelectTrigger size={size} aria-label={label} className={cn("min-w-0 max-w-full rounded-lg font-normal", size === "default" ? "type-callout" : "type-footnote", quiet ? "-ml-px bg-transparent px-2 shadow-none! hover:bg-accent [&_.ui-selc__chevron]:opacity-0 hover:[&_.ui-selc__chevron]:opacity-100" : "text-foreground", className)} data-select={testId}><SelectValue placeholder={placeholder} className="min-w-0 overflow-hidden truncate" /></SelectTrigger>
      <SelectContent align={align === "end" ? "end" : "start"} className="min-w-44">{options.map((o) => <SelectItem key={o.v} value={o.v} disabled={o.disabled}>{o.l}</SelectItem>)}</SelectContent>
    </UISelect>
  )
}

export const selected = new Set<string>();

/** the count after a section heading's label, one step quieter than the label */
export const Count = ({ n }: { n: number }) => (n ? <span className="ml-2 font-normal tabular-nums text-tertiary/80">{n}</span> : null)

/** whether an item shows plan progress in a list row (open items with any plan, or closed items with unfinished steps) */
export function itemShowsPlan(i: Item): boolean {
  const total = i.planTotal ?? i.plan?.length ?? 0
  if (!total) return false
  if (!closedSt(i.st)) return true
  const done = i.planDone ?? i.plan?.filter((x) => x[1] === 1).length ?? 0
  return total > done || !!i.plan?.some((x) => x[1] !== 1)
}

/** plan progress as a count with a short bar; on a finished item only when steps were left open, which is worth a look */
export function PlanBar({ i, warn, className }: { i: Item; warn?: boolean; className?: string }) {
  const t = useT()
  const total = i.planTotal ?? i.plan?.length ?? 0
  const dn = i.planDone ?? i.plan?.filter((x) => x[1] === 1).length ?? 0
  if (!total) return null
  return (
    <span className={cn("inline-flex w-full items-center justify-end gap-1.5 whitespace-nowrap tabular-nums", warn && "text-attention", className)} title={t("shell:item.plan", { done: dn, total })} data-plan>
      <span className="text-right">{dn}/{total}</span>
      <Progress value={(dn / total) * 100} className={cn("flex w-8 shrink-0 [&_[data-slot=progress-track]]:h-1", warn ? "[&_[data-slot=progress-indicator]]:bg-attention" : "[&_[data-slot=progress-indicator]]:bg-foreground/60")} />
    </span>
  )
}

/** one item in a list. Open items take two lines: the title with aligned metadata columns on the right (kind, project,
 *  plan, execution, time), then what it is doing now (plus due date and dependency chips). Finished and dropped items
 *  fold into one line, their outcome after the title. `proj` / `bot` / `kind` / `plan` off: omit that fixed column
 *  when the list is already scoped or no item has a value for it */
export const ItemRow = memo(function ItemRow({ i, pick, proj = true, bot = true, kind: showKind = true, plan: showPlan = true, badge }: { i: Item; pick?: () => void; proj?: boolean; bot?: boolean; kind?: boolean; plan?: boolean; badge?: ReactNode }) {
  const t = useT()
  const on = selected.has(i.id), closed = closedSt(i.st), w = i.wait
  const executions = useItemRuntime(i.id)
  const hasPlan = itemShowsPlan(i)
  const dueChip = !closed && i.due ? <span className={cn("shrink-0 whitespace-nowrap", /今天/.test(i.due) && "text-foreground/80")} data-item-due>{t("shell:item.due", { due: dueLabel(i.due) })}</span> : null
  const extra = [
    i.ss.length > 1 && <span key="ss" className="shrink-0 whitespace-nowrap">{t("shell:item.sessions", { count: i.ss.length })}</span>,
    (i.queued ?? i.q.length) > 0 && <span key="q" className="shrink-0 whitespace-nowrap">{t("shell:item.queued", { count: i.queued ?? i.q.length })}</span>,
    i.parent && <span key="p" className="inline-flex shrink-0 items-center gap-1 whitespace-nowrap"><GitBranchIcon className="size-3" />{t("shell:item.forked")}</span>,
    /* an open item names what it is still waiting for; once nothing is in its way the relation is history and stays on the item page */
    !closed && depsOpen(i).length > 0 && <span key="d" className="shrink-0 whitespace-nowrap" title={depsOpen(i).map((d) => `${d} ${getItem(d)?.t ?? ""}`).join("\n")} data-waits-on>{depsOpen(i).length > 1 ? t("shell:item.waitsOnMany", { id: depsOpen(i)[0], count: depsOpen(i).length }) : t("shell:item.waitsOn", { id: depsOpen(i)[0] })}</span>,
  ].filter(Boolean)
  /* a list row carries one tag, its kind, as quiet text (colour stays for status); the rest are in its hover title and
   * on the item page */
  const kind = kindOf(i) ? tag(kindOf(i)!) : undefined
  const others = (i.tags ?? []).map(tag).filter((x) => x && !isKind(x)).map((x) => tagLabel(x!))
  const side = (
    <span className="col-start-4 row-start-1 flex h-[calc(22px*var(--type-scale))] shrink-0 items-center gap-2.5 pl-2 type-footnote text-muted-foreground" data-item-side>
      {showKind && (
        <span data-item-col="kind" className="inline-flex shrink-0 items-center overflow-hidden">
          {kind && <span className="min-w-0 truncate text-tertiary" title={[kind.name, ...others].join(" · ")} data-kind={kind.name}>{kind.name}</span>}
        </span>
      )}
      {proj && (
        <span data-item-col="project" className="inline-flex shrink-0 items-center overflow-hidden">
          {i.project ? <ProjectChip id={i.project} plain className="min-w-0 max-w-full" /> : null}
        </span>
      )}
      {showPlan && (
        <span data-item-col="plan" className="inline-flex shrink-0 items-center justify-end">
          {hasPlan ? <PlanBar i={i} warn={closed} /> : null}
        </span>
      )}
      <span data-item-col="exec" className="inline-flex shrink-0 items-center gap-1 whitespace-nowrap">
        {executions.length > 0 ? (
          <>
            <SessionIdentity session={executions[0]} compact />
            {executions.length > 1 && (
              <span className="shrink-0 tabular-nums" title={executions.map((s) => [botName(s.b), s.h ?? t("core:unknownHarness"), s.m ?? t("core:unknownModel")].join(" · ")).join("\n")}>
                +{executions.length - 1}
              </span>
            )}
          </>
        ) : bot && i.b ? (
          <ActorAvatar actorId={i.b} size={18} className="inline-flex" />
        ) : null}
      </span>
      <span data-item-col="time" className="inline-flex shrink-0 items-center justify-end whitespace-nowrap text-right tabular-nums text-tertiary">{i.up}</span>
    </span>
  )
  return (
    <div role="button" tabIndex={0} onClick={() => (pick ? pick() : go("#/items/" + i.id))} onKeyDown={(e) => e.key === "Enter" && go("#/items/" + i.id)} data-item-row={i.id} data-item-status={itemDisplayStatus(i)} data-dense={closed || undefined}
      className={cn("group relative grid w-full cursor-pointer grid-cols-[auto_auto_minmax(0,1fr)_auto] items-center gap-x-2.5 rounded-lg px-2 transition-colors duration-120 hover:bg-accent", closed ? "py-1.5" : "py-2.5", pick && on && "bg-blue/10 hover:bg-blue/10")}>
      <span className="col-start-1 row-start-1 flex h-[calc(22px*var(--type-scale))] items-center justify-center" data-row-indicator>
        {pick ? <Checkbox checked={on} aria-label={t("shell:item.select", { id: i.id })} data-pick={i.id} /> : <StatusDot s={itemDisplayStatus(i)} />}
      </span>
      <ItemNo id={i.id} className="col-start-2 row-start-1 inline-flex h-[calc(22px*var(--type-scale))] min-w-[7.5ch] items-center" />
      <div className="col-start-3 row-start-1 flex min-h-[calc(22px*var(--type-scale))] min-w-0 items-center gap-2">
        <span className={cn("min-w-0 truncate type-body", closed ? "text-foreground/85" : "font-medium")} data-item-title>{i.t}</span>
        {i.star && <StarIcon className="size-3.5 shrink-0 fill-current text-tertiary" />}
        {i.pri === "高" && !closed && <span className="inline-flex h-5 shrink-0 items-center whitespace-nowrap rounded-md bg-attention/12 px-1.5 type-footnote font-medium text-attention" data-item-pri="high">{t("shell:item.high")}</span>}
        {badge}
        {closed && i.progress && <span className="min-w-0 shrink-[2] truncate type-footnote text-muted-foreground" data-item-now data-item-closed-now>{i.progress}</span>}
      </div>
      {side}
      {/* a card waiting on you says what it asks and takes you to it; otherwise the row says what is going on now */}
      {!closed && w && (
        <div className="col-span-2 col-start-3 mt-0.5 flex min-w-0 items-center gap-2">
          <Meta one className="min-w-0 flex-1 overflow-hidden [&>*:first-child]:min-w-0 [&>*:first-child]:truncate"><span className="type-callout text-foreground/80" data-item-wait={w.type}>{waitLine(w)}{i.waits.length > 1 && <span className="text-muted-foreground">{t("shell:item.andMore", { count: i.waits.length })}</span>}</span>{dueChip}{extra}</Meta>
          {!pick && <Button size="xs" variant="outline" className="shrink-0" onClick={(e) => { e.stopPropagation(); go("#/items/" + i.id) }} data-item-wait-go={i.id}>{t("shell:item.goAct", { act: WACT[w.type] ?? t("shell:item.handle") })}</Button>}
        </div>
      )}
      {!closed && !w && (i.progress || dueChip || extra.length > 0) && (
        <Meta one className="col-span-2 col-start-3 mt-0.5">
          {i.progress && <span className="min-w-0 truncate type-callout" data-item-now>{i.progress}</span>}
          {dueChip}
          {extra}
        </Meta>
      )}
      {i.rel && i.rel.tone !== "ok" && <RelNote rel={i.rel} className="col-span-2 col-start-3 mt-1" onClose={i.rel.tone === "fail" ? () => closeRel(i) : undefined} />}
    </div>
  )
})

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
