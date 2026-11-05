import { ChevronRightIcon, ShieldCheckIcon } from "lucide-react"
import { cn } from "cn"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import type { RDecision } from "@/api"
import { permissionStatus } from "@/lib/permission"
import { useT } from "@/i18n"

/** Preserve the complete request while keeping long commands out of the reading flow. */
export function PermissionDetails({ title, input }: { title: string; input?: string }) {
  const t = useT()
  return <div className="overflow-hidden rounded-lg border bg-muted/40" data-permission-details>
    <div className="border-b px-3 py-1.5 type-footnote text-muted-foreground">{t("misc:permission.request")}</div>
    <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-all px-3 py-2.5 font-mono text-[12px] leading-5">{title}</pre>
    {input && input !== "{}" && <details className="border-t">
      <summary className="cursor-pointer px-3 py-2 type-footnote text-muted-foreground">{t("misc:permission.toolArgs")}</summary>
      <pre className="max-h-56 overflow-auto whitespace-pre-wrap break-all px-3 pb-3 font-mono text-[12px] leading-5">{input}</pre>
    </details>}
  </div>
}

export function PermissionRecord({ title, decision, id }: { title: string; decision?: RDecision; id?: string }) {
  const t = useT(), status = permissionStatus(decision)
  return <Collapsible className={cn("min-w-0 rounded-lg border", status.pending ? "border-attention/30 bg-attention/5" : "border-border/60")} data-permission-record data-permission-status={status.label}>
    <div className="flex min-w-0 items-center gap-2 px-3 py-2">
      <CollapsibleTrigger className="group/permission flex min-w-0 flex-1 items-center gap-2 text-left type-callout">
        <ShieldCheckIcon className={cn("size-4 shrink-0", status.pending ? "text-attention" : "text-muted-foreground")} />
        <span className="shrink-0 font-medium">{t("misc:permission.title")}</span>
        <span className="min-w-0 truncate font-mono text-[12px] text-muted-foreground">{title}</span>
        <ChevronRightIcon className="ml-auto size-3.5 shrink-0 text-muted-foreground transition-transform group-aria-expanded/permission:rotate-90" />
      </CollapsibleTrigger>
      <span className={cn("shrink-0 type-footnote", status.pending ? "font-medium text-attention" : "text-muted-foreground")}>{status.label}</span>
    </div>
    <CollapsibleContent>
      <div className="px-3 pb-3">
        <PermissionDetails title={title} input={decision?.draft} />
        {status.pending && id && <a href="#/inbox" className="mt-2 inline-flex min-h-8 items-center type-callout font-medium text-blue hover:underline">{t("misc:permission.goInbox")}</a>}
      </div>
    </CollapsibleContent>
  </Collapsible>
}
