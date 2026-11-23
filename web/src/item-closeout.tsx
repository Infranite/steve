import type { Closeout } from "@/api"
import { useT } from "@/i18n"
import { date } from "@/i18n/format"
import { cn } from "@/lib/utils"

/** Live snapshot, not a new timeline entry for every poll. Reused by the item and conversation. */
export function CloseoutSummary({ value: c, compact, className }: { value: Closeout; compact?: boolean; className?: string }) {
  const t = useT(), pr = c.pr, checks = pr?.checks
  const ts = c.last_success_at == null ? null : new Date(c.last_success_at)
  const url = pr?.url && /^https?:\/\//i.test(pr.url) ? pr.url : null
  const ref = pr && <>{compact || !url ? <span>PR #{pr.number}</span> : <a href={url} target="_blank" rel="noreferrer" className="font-medium hover:underline">PR #{pr.number}</a>}
    {pr.head && <code title={pr.head} data-pr-head>{pr.head.slice(0, 12)}</code>}</>
  return (
    <div className={cn("min-w-0 space-y-1 type-footnote text-muted-foreground", className)} data-closeout={c.status}>
      <div className="flex flex-wrap items-baseline gap-x-2">
        <span className="font-medium text-foreground" data-closeout-status>{t(`views:closeout.status.${c.status}`, { defaultValue: c.status })}</span>
        {ref}
      </div>
      <div className="flex flex-wrap gap-x-3 gap-y-0.5">
        {pr && <span data-closeout-checks>{checks
          ? checks.passed + checks.pending + checks.failed + checks.other > 0
            ? t("views:closeout.checks", checks) : t("views:closeout.noChecks")
          : t("views:closeout.unknownChecks")}</span>}
        <span data-closeout-owner>{t("views:closeout.nextOwner", { owner: t(`views:closeout.owner.${c.next_owner}`, { defaultValue: c.next_owner }) })}</span>
        <span data-closeout-sync>{ts ? <time dateTime={ts.toISOString()}>{t("views:closeout.lastSync", { time: date(ts, { dateStyle: "short", timeStyle: "medium" }) })}</time>
          : t("views:closeout.neverSynced")}</span>
      </div>
      {!!checks?.failing.length && <p className="[overflow-wrap:anywhere]" data-closeout-failing>{t(c.sync_error ? "views:closeout.lastFailures" : "views:closeout.failures", { names: checks.failing.join(" · ") })}</p>}
      {c.sync_error && <p className="text-attention [overflow-wrap:anywhere]" data-closeout-error>{t("views:closeout.syncError", { reason: c.sync_error })}</p>}
      {c.merge_error && <p className="text-attention [overflow-wrap:anywhere]" data-closeout-error>{t("views:closeout.mergeError", { reason: c.merge_error })}</p>}
    </div>
  )
}
