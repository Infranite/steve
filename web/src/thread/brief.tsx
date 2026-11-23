import { cn } from "cn"
import { ChevronRightIcon } from "lucide-react"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { Markdown } from "@/components/markdown"
import { briefHead } from "@/store"

/* a prompt Steve hands an agent is not a message: the task brief (role, rules, retrieved knowledge) folds to one line, its text kept verbatim */
export function Brief({ text, label, className }: { text: string; label?: string; className?: string }) {
  const head = briefHead(text), who = label && <span className="mr-1.5 font-medium text-foreground/80">{label}</span>
  if (head === text.trim()) return <div className={cn("break-words", className)}>{who}{text}</div>
  return (
    <Collapsible className={cn("min-w-0", className)} data-brief>
      <CollapsibleTrigger className="group/brief flex max-w-full items-center gap-1 text-left hover:text-foreground">
        <span className="min-w-0 truncate">{who}{head}</span><ChevronRightIcon className="size-3.5 shrink-0 transition-transform duration-180 ease-(--ease-out) group-data-[panel-open]/brief:rotate-90" />
      </CollapsibleTrigger>
      <CollapsibleContent><div className="mt-1.5 max-h-80 overflow-auto break-words border-l-2 border-border py-0.5 pl-3 text-muted-foreground" data-brief-body><Markdown text={text} className="type-footnote" /></div></CollapsibleContent>
    </Collapsible>
  )
}
