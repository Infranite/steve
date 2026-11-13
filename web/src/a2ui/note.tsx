/* The user message an A2UI button made (ST-46): what was submitted, in a line, instead of the action JSON the model reads */
import { MousePointerClickIcon } from "lucide-react"
import type { A2uiRecord } from "@/api"
import { str } from "@/a2ui/model"
import { useT } from "@/i18n"

export function A2uiActionNote({ a }: { a: A2uiRecord }) {
  const t = useT()
  const fields = Object.entries(a.context ?? {}).filter(([, v]) => v !== undefined && v !== null && v !== "").slice(0, 6)
  return (
    <div className="flex max-w-[80%] flex-col items-end gap-1 self-end anim-in" data-me-msg data-a2ui-action={a.name}>
      <div className="flex min-w-0 max-w-full items-center gap-1.5 rounded-2xl bg-muted px-4 py-2.5 type-callout">
        <MousePointerClickIcon className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
        <span className="min-w-0 truncate">{t("misc:a2ui.submittedNote", { label: a.label || a.name })}</span>
      </div>
      {fields.length > 0 && <dl className="flex max-w-full flex-wrap justify-end gap-x-3 gap-y-0.5 px-1 type-footnote text-muted-foreground">
        {fields.map(([k, v]) => <div key={k} className="flex min-w-0 gap-1"><dt className="shrink-0">{k}</dt><dd className="min-w-0 truncate text-foreground/80">{Array.isArray(v) ? v.map(str).join(t("misc:a2ui.listSep")) : str(v)}</dd></div>)}
      </dl>}
    </div>
  )
}
