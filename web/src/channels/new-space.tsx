/* The 待处理 card “新群” (channels-personas.md §2.2, domain-model.md §1.4): `{kind:"ask", subject_type:"space",
 * options:["open","mute","quiet","block"]}` (older cards only have open / mute) — “<账号>被拉进了「<群>」，要设置它在这里做什么吗？”.
 * 不提醒 and 屏蔽 are your attention (records without asking you / stops processing); 只记录 is Steve's role there (the
 * old 静默). A space that looks like a notification group gets 不提醒 suggested, never applied by itself. On a card
 * without the new options the page answers `open` and then sets the attention itself. Rendered from ui.tsx's
 * DecisionCard with its header, footer and `act`. */
import { useEffect, useState, type ReactNode } from "react"
import { BellOffIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { CardContent } from "@/components/ui/card"
import { go } from "@/ui"
import { report } from "@/accounts/load"
import type { Wait } from "@/store"
import { channelsApi, type Attention, type Space } from "@/api/channels"
import { SpaceAvatar, kindLine } from "@/channels/space-bits"
import { useT } from "@/i18n"

type P = { w: Wait; act: (c: string, t?: string) => Promise<void>; busy: boolean; head: (t: ReactNode) => ReactNode; foot: (c: ReactNode) => ReactNode }

export function NewSpaceCard({ w, act, busy, head, foot }: P) {
  const t = useT()
  const [s, setS] = useState<Space | null>(null), sid = w.subjectId ?? ""
  useEffect(() => { if (!sid) return; let live = true; channelsApi.space(sid).then((x) => live && setS(x), () => {}); return () => { live = false } }, [sid])
  const level = (a: Attention, choice: "quiet" | "block") => w.options.includes(choice)
    ? void act(choice).catch(report)
    : void act("open").then(() => channelsApi.patchSpace(sid, { attention: a })).catch(report)
  const suggest = s?.suggest_quiet
  return <>
    {head(w.q)}
    <CardContent className="flex flex-col gap-3 pb-4" data-new-space={sid}>
      {s && <div className="flex min-w-0 items-center gap-2.5"><SpaceAvatar s={s} size={28} /><div className="min-w-0"><div className="truncate type-body" title={s.title || s.name}>{s.title || s.name}</div>
        <div className="truncate type-footnote text-muted-foreground">{[kindLine(s), s.has_external ? t("channels:word.externalMembers") : "", t("channels:newSpace.current", { persona: s.persona?.name ?? t("channels:word.defaultPersona") })].filter(Boolean).join(" · ")}</div></div></div>}
      {suggest && <div className="flex items-start gap-2 rounded-lg bg-blue/8 px-3 py-2 type-footnote" data-suggest-quiet><BellOffIcon className="mt-px size-3.5 shrink-0 text-blue" aria-hidden />
        <span className="min-w-0">{t("channels:newSpace.suggest", { reason: suggest })}</span></div>}
      <div className="type-footnote text-muted-foreground">{t("channels:newSpace.legend")}</div>
    </CardContent>
    {foot(<>
      <Button variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => level("blocked", "block")} data-space-block>{t("channels:newSpace.block")}</Button>
      {w.options.includes("mute") && <Button variant="outline" disabled={busy} onClick={() => void act("mute").then(() => channelsApi.patchSpace(sid, { muted: true })).catch(report)} data-space-mute>{t("channels:word.recordOnly")}</Button>}
      <Button variant={suggest ? "default" : "outline"} disabled={busy} onClick={() => level("quiet", "quiet")} data-space-quiet>{t("channels:newSpace.quiet")}</Button>
      {w.options.includes("open") && <Button variant={suggest ? "outline" : "default"} disabled={busy} onClick={() => void act("open").then(() => go("#/channels/space/" + sid))} data-space-open>{t("channels:newSpace.open")}</Button>}
    </>)}
  </>
}
