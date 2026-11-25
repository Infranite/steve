import { RocketIcon } from "lucide-react"
import { Trans } from "react-i18next"
import { useT } from "@/i18n"
import { st, useShellSelector } from "@/store"

/** while the Hub holds new turns for a release: what it waits for, and that new messages go after it */
export function HoldBanner() {
  const h = useShellSelector(() => st.hold), t = useT()
  if (!h) return null
  const wait = h.running > 0 ? t("shell:queue.waitTurns", { count: h.running }) : t("shell:queue.restartNow")
  return (
    <div role="status" className="flex shrink-0 items-center gap-2 border-b bg-blue/8 px-4 py-2 type-footnote text-foreground/80 sm:px-6" data-hold>
      <RocketIcon className="size-3.5 shrink-0 text-blue" aria-hidden />
      <span className="min-w-0"><Trans t={t} i18nKey="shell:queue.hold" values={{ item: h.item, wait }} components={{ link: <a href={"#/items/" + h.item} className="font-mono tabular-nums underline-offset-2 hover:underline" /> }} /></span>
    </div>
  )
}
