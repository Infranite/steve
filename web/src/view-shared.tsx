import { Checkbox } from "@/components/ui/checkbox"
import { Field, FieldContent, FieldDescription, FieldLabel, FieldTitle } from "@/components/ui/field"
import { Progress } from "@/components/ui/progress"
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group"
import { SelectContent, SelectItem, SelectTrigger, SelectValue, Select as UISelect } from "@/components/ui/select"
import { botName, closeRel, depsOpen, st, type Item } from "@/store"
import { BotAvatar, go, ItemNo, Meta, RelNote, StatusDot, Tags } from "@/ui"
import { cn } from "cn"
import { FolderIcon, GitBranchIcon, StarIcon } from "lucide-react"
import { type ReactNode } from "react"

/** the item's project as a link; items without one say so, since the work then happens in a scratch directory */
export function ProjectChip({ id, className, plain }: { id: string | null; className?: string; plain?: boolean }) {
  const p = st.projects.find((x) => x.id === id)
  if (!p) return <span className={cn("inline-flex items-center gap-1", className)} data-project-chip="none"><FolderIcon className="size-3.5" strokeWidth={1.75} />没有项目</span>
  /* plain: inside a clickable row or card, where a nested link would be a control inside a control */
  if (plain) return <span className={cn("inline-flex min-w-0 items-center gap-1", className)} data-project-chip={p.id}><FolderIcon className="size-3.5 shrink-0" strokeWidth={1.75} /><span className="truncate">{p.name}</span></span>
  return <a href={"#/projects/" + p.id} onClick={(e) => e.stopPropagation()} className={cn("inline-flex items-center gap-1 text-foreground/80 hover:text-foreground hover:underline", className)} data-project-chip={p.id}><FolderIcon className="size-3.5" strokeWidth={1.75} />{p.name}</a>
}

export function Select({ value, options, onChange, align = "start", testId, quiet, className }: { value: string; options: { v: string; l: ReactNode }[]; onChange: (v: string) => void; align?: "start" | "end"; testId?: string; quiet?: boolean; className?: string }) {
  return (
    <UISelect items={Object.fromEntries(options.map((o) => [o.v, o.l]))} value={value} onValueChange={(v) => v != null && onChange(String(v))}>
      <SelectTrigger size="sm" className={cn("rounded-lg bg-transparent font-normal", quiet ? "-ml-px max-w-full border-transparent px-2 hover:bg-accent [&>svg]:opacity-0 hover:[&>svg]:opacity-100" : "border-border", className)} data-select={testId}><SelectValue /></SelectTrigger>
      <SelectContent align={align === "end" ? "end" : "start"} className="min-w-44">{options.map((o) => <SelectItem key={o.v} value={o.v}>{o.l}</SelectItem>)}</SelectContent>
    </UISelect>
  )
}

export const selected = new Set<string>();

/** plan progress as a count with a short bar; on a finished item only when steps were left open, which is worth a look */
export function PlanBar({ i, warn }: { i: Item; warn?: boolean }) {
  if (!i.plan?.length) return null
  const dn = i.plan.filter((x) => x[1] === 1).length
  return <span className={cn("inline-flex items-center gap-1.5 tabular-nums", warn && "text-attention")} title={`计划 ${dn}/${i.plan.length}`} data-plan>{dn}/{i.plan.length}<Progress value={(dn / i.plan.length) * 100} className={cn("hidden w-8 sm:flex [&_[data-slot=progress-track]]:h-1", warn ? "[&_[data-slot=progress-indicator]]:bg-attention" : "[&_[data-slot=progress-indicator]]:bg-foreground/60")} /></span>
}

/** one item in a list. Open items take two lines: the title with what tells it apart on the right (tags, due, plan,
 *  bot, time), then what it is doing now. Finished and dropped items fold into one line, their outcome after the title.
 *  `proj` / `bot` off: the list is already about one project or bot, so the row doesn't repeat it */
export function ItemRow({ i, pick, proj = true, bot = true }: { i: Item; pick?: () => void; proj?: boolean; bot?: boolean }) {
  const on = selected.has(i.id), closed = i.st === "done" || i.st === "drop"
  const planOpen = !!i.plan?.length && i.plan.some((x) => x[1] !== 1)
  const extra = [
    i.ss.length > 1 && <span key="ss">{i.ss.length} 个会话</span>,
    i.q.length > 0 && <span key="q">{i.q.length} 条补充排队</span>,
    i.parent && <span key="p" className="inline-flex items-center gap-1"><GitBranchIcon className="size-3" />派生</span>,
    !!i.deps?.length && <span key="d">前置 {i.deps.length - depsOpen(i).length}/{i.deps.length}</span>,
  ].filter(Boolean)
  const side = (
    <span className="ml-auto flex shrink-0 items-center gap-2.5 pl-2 type-footnote text-muted-foreground">
      <Tags ids={i.tags} max={2} className="hidden flex-nowrap sm:inline-flex" />
      {i.pri === "高" && !closed && <span className="rounded bg-attention/12 px-1.5 text-attention">高</span>}
      {i.due && !closed && <span className={cn("whitespace-nowrap", /今天/.test(i.due) && "text-foreground/80")}>截止 {i.due}</span>}
      {proj && i.project && <span className="hidden max-w-28 md:inline-flex"><ProjectChip id={i.project} plain /></span>}
      {(!closed || planOpen) && <PlanBar i={i} warn={closed} />}
      {bot && i.b && <span title={botName(i.b)} className="hidden sm:inline-flex"><BotAvatar k={i.b} size={18} /></span>}
      <span className="min-w-11 whitespace-nowrap text-right tabular-nums">{i.up}</span>
    </span>
  )
  return (
    <div role="button" tabIndex={0} onClick={() => (pick ? pick() : go("#/items/" + i.id))} onKeyDown={(e) => e.key === "Enter" && go("#/items/" + i.id)} data-item-row={i.id} data-dense={closed || undefined}
      className={cn("group flex cursor-pointer gap-2.5 rounded-lg px-2 transition-colors duration-150 hover:bg-accent", closed ? "items-center py-1.5" : "items-start py-2.5", pick && on && "bg-blue/5")}>
      {pick ? <Checkbox checked={on} className={cn(!closed && "mt-[3px]")} aria-label={"选择 " + i.id} data-pick={i.id} /> : <StatusDot s={i.st} className={cn(!closed && "mt-[7px]")} />}
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 items-center gap-2">
          <ItemNo id={i.id} className="w-12" />
          <span className={cn("truncate type-body", closed ? "text-foreground/85" : "font-medium")}>{i.t}</span>
          {i.star && <StarIcon className="size-3.5 shrink-0 fill-current text-tertiary" />}
          {closed && i.now && <span className="hidden min-w-0 shrink-[2] truncate type-footnote text-muted-foreground sm:block" data-item-now>{i.now}</span>}
          {side}
        </div>
        {!closed && (i.now || extra.length > 0) && (
          <Meta className="mt-0.5 flex-nowrap pl-14 [&>*:first-child]:min-w-0 [&>*:first-child]:truncate"><span className="type-callout" data-item-now>{i.now || "—"}</span>{extra}</Meta>
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