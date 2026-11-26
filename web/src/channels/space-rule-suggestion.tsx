/* Space rule-suggestion card (`kind: "propose"`, `subject_type: "space"`, options `["record_only", "keep"]`):
 * opened when similar messages in a space are ignored repeatedly within 7 days (#213). */
import { useEffect, useState, type ReactNode } from "react"
import { Button } from "@/components/ui/button"
import { CardContent } from "@/components/ui/card"
import { go } from "@/ui"
import { report } from "@/accounts/load"
import type { Wait } from "@/store"
import { channelsApi, spaceTitle, type Space } from "@/api/channels"
import { SpaceAvatar, kindLine } from "@/channels/space-bits"
import { useT } from "@/i18n"

type P = { w: Wait; act: (c: string, t?: string) => Promise<void>; busy: boolean; head: (t: ReactNode) => ReactNode; foot: (c: ReactNode) => ReactNode }

export function SpaceRuleSuggestionCard({ w, act, busy, head, foot }: P) {
  const t = useT()
  const [s, setS] = useState<Space | null>(null), sid = w.subjectId ?? ""
  useEffect(() => {
    if (!sid) return
    let live = true
    channelsApi.space(sid).then((x) => live && setS(x), () => {})
    return () => { live = false }
  }, [sid])
  return <>
    {head(w.q)}
    <CardContent className="flex flex-col gap-3 pb-4" data-space-rule-suggestion={sid}>
      {s && <div className="flex min-w-0 items-center gap-2.5">
        <SpaceAvatar s={s} size={28} />
        <div className="min-w-0">
          <div className="truncate type-body" title={spaceTitle(s)}>{spaceTitle(s)}</div>
          <div className="truncate type-footnote text-muted-foreground">{[kindLine(s), s.has_external ? t("channels:word.externalMembers") : ""].filter(Boolean).join(" · ")}</div>
        </div>
      </div>}
      <div className="type-footnote text-muted-foreground">{t("channels:ruleSuggestion.note")}</div>
    </CardContent>
    {foot(<>
      {sid && <Button variant="ghost" className="mr-auto text-muted-foreground" disabled={busy} onClick={() => go("#/channels/space/" + sid)} data-space-rule-settings>{t("channels:ruleSuggestion.openSettings")}</Button>}
      <Button variant="outline" disabled={busy} onClick={() => void act("keep").catch(report)} data-space-rule-keep>{t("channels:ruleSuggestion.keep")}</Button>
      <Button disabled={busy} onClick={() => void act("record_only").catch(report)} data-space-rule-record>{t("channels:ruleSuggestion.recordOnly")}</Button>
    </>)}
  </>
}
