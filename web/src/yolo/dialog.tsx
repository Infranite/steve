import { useEffect, useState } from "react"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Spinner } from "@/components/ui/spinner"
import { yoloApi, type RYoloPreview, type YoloLevel } from "@/api/yolo"
import { st, yoloResume, yoloSet } from "@/store"
import { closeSheet, go } from "@/ui"
import { choiceText } from "@/yolo/badges"
import { Dot, fullGives, fullKeeps, label, levelOf, levels, never, yoloState } from "@/yolo/switch"
import { AutonomyDial, type AgentAction, type AutonomyLevel, type DialText } from "@/components/affect/autonomy-dial"
import { Trans } from "react-i18next"
import { t, useT } from "@/i18n"

const LEVEL_NO: Record<YoloLevel, AutonomyLevel> = { off: 0, on: 1, full: 2 }
const NO_LEVEL: YoloLevel[] = ["off", "on", "full"]
type WithDetail = "cmd" | "accept" | "continue" | "ask" | "release" | "botReply" | "aiSend" | "anyCmd"
const A = (id: WithDetail, risk: AgentAction["risk"]): AgentAction => ({ id, risk, label: t(`yolo:dial.action.${id}`), detail: t(`yolo:dial.action.${id}Detail`) })
const B = (id: "propose" | "anyFile" | "pay" | "vault" | "send" | "otherRelease", risk: AgentAction["risk"]): AgentAction => ({ id, risk, label: t(`yolo:dial.action.${id}`) })
/** What each level hands over, in the Hub's words (judge.rs `Policy::default`, the hard limits and what full access
 *  keeps): safe ones YOLO decides, risky ones only full access lets through, and the locked ones always come to you. */
export const dialActions = (): AgentAction[] => [
  A("cmd", "safe"), A("accept", "safe"), A("continue", "safe"), A("ask", "safe"), B("propose", "safe"), A("release", "safe"),
  A("botReply", "safe"), A("aiSend", "safe"), A("anyCmd", "risky"), B("anyFile", "risky"),
  B("pay", "destructive"), B("vault", "destructive"), B("send", "destructive"), B("otherRelease", "destructive"),
]
export const LOCKED = ["pay", "vault", "send", "otherRelease"]
export const dialText = (): Partial<DialText> => ({
  levels: levels().map((l) => ({ label: l.m === "off" ? l.title : label(l.m), hint: l.sub })) as DialText["levels"],
  group: t("yolo:switch.modes"), own: t("yolo:dial.own"), ask: t("yolo:dial.ask"), ownEmpty: t("yolo:dial.ownEmpty"), askEmpty: t("yolo:dial.askEmpty"),
  risky: t("yolo:dial.risky"), destructive: t("yolo:dial.destructive"), current: t("yolo:switch.current"), locked: t("yolo:dial.locked"),
  announce: (level, own, ask) => t("yolo:dial.announce", { level, own, ask }),
})

/** the control (sheet kind `yolo`) */
export function YoloDialog() {
  const t = useT()
  const y = st.yolo, s = yoloState(y), cur = levelOf(y)
  const [sel, setSel] = useState<YoloLevel>(cur)
  const [pv, setPv] = useState<RYoloPreview | null>(null), [err, setErr] = useState(""), [all, setAll] = useState(false), [busy, setBusy] = useState(false), [ok, setOk] = useState(false)
  const turningOn = cur === "off" && sel !== "off" && s !== "forced" && s !== "paused"
  useEffect(() => {
    if (!turningOn || pv) return
    yoloApi.preview().then(setPv).catch((e) => setErr((e as Error).message))
  }, [turningOn, pv])
  if (!y || !s) return null
  const run = async (f: () => Promise<unknown>) => { setBusy(true); try { await f() } finally { setBusy(false) } }
  const n = pv?.would_handle.length ?? 0
  const locked = s === "forced" || s === "paused"
  const action = sel === cur ? null : sel === "off" ? t("yolo:switch.turnOff") : sel === "on" ? (cur === "full" ? t("yolo:switch.backToYolo") : t("yolo:switch.turnOnYolo")) : t("yolo:switch.turnOnFull")
  const needsOk = sel === "full" && cur !== "full"
  return (
    <Dialog open onOpenChange={(o) => !o && closeSheet()}>
      <DialogContent className="max-h-[calc(100dvh-2rem)] grid-rows-[auto_minmax(0,1fr)_auto] gap-4 rounded-2xl p-4 [overflow-wrap:anywhere] sm:max-w-[520px] sm:p-6" data-yolo-dialog={s}>
        <DialogHeader className="min-w-0 pr-8">
          <DialogTitle className="type-title">{t("yolo:switch.title")}</DialogTitle>
          <DialogDescription className="type-callout">{t("yolo:switch.desc")}</DialogDescription>
        </DialogHeader>
        <div className="flex min-h-0 min-w-0 flex-col gap-4 overflow-y-auto p-1 *:shrink-0" data-yolo-scroll>
          {s === "forced" && <div className="rounded-xl bg-muted/60 px-4 py-3 type-callout text-muted-foreground" data-yolo-forced>{t("yolo:switch.forced", { reason: y.forced_reason })}</div>}
          {s === "paused" && <div role="alert" className="flex flex-wrap items-start gap-2.5 rounded-xl bg-destructive/10 px-4 py-3 type-callout" data-yolo-paused>
            <Dot s="paused" className="mt-[7px]" />
            <div className="min-w-0 flex-1"><div className="font-medium">{label("paused")}</div><div className="mt-0.5 text-muted-foreground">{t("yolo:switch.pausedNote", { reason: y.paused?.reason })}</div></div>
            <Button size="sm" disabled={busy} onClick={() => void run(async () => { await yoloResume(); closeSheet() })} data-yolo-resume>{t("yolo:run.resume")}</Button></div>}
          {locked && cur !== "off" && <div className="flex flex-wrap items-center gap-x-1.5 type-footnote text-muted-foreground" data-yolo-stored={cur}>
            <span>{t("yolo:switch.storedLocked", { level: label(cur) })}</span>
            {sel !== "off" && <><span>·</span><button type="button" className="text-blue hover:underline" onClick={() => setSel("off")} data-yolo-stored-off>{t("yolo:switch.storedOff")}</button></>}</div>}
          {/* forced off or paused: nothing is handled for you, so the dial shows 关 whatever mode is stored */}
          <AutonomyDial className="[--ui-adlc-max:100%]" title="" actions={dialActions()} pinned={LOCKED} lockable={false} text={dialText()} bullets={false}
            level={locked ? 0 : LEVEL_NO[sel]} onLevelChange={(n) => { setSel(NO_LEVEL[n]); setOk(false) }} currentLevel={locked ? undefined : LEVEL_NO[cur]}
            disabledLevels={locked ? [1, 2] : []} tones={{ 2: "attention" }} levelProps={(n) => ({ "data-yolo-level": NO_LEVEL[n] })}>
            {(cur === "on" || cur === "full") && !locked && <div className="flex flex-wrap items-center gap-x-1.5 type-footnote text-muted-foreground" data-yolo-on>
            <span><Trans t={t} i18nKey={y.counts.scheduled ? "yolo:switch.countsScheduled" : "yolo:switch.counts"} values={{ decided: y.counts.decided, declined: y.counts.declined, scheduled: y.counts.scheduled }} components={{ b: <b className="font-medium text-foreground" /> }} /></span>
            <span>·</span><button type="button" className="text-blue hover:underline" onClick={() => { closeSheet(); go("#/yolo") }} data-yolo-digest-link>{t("yolo:switch.viewRecord")}</button></div>}
          {turningOn && <div className="anim-in flex min-w-0 shrink-0 flex-col gap-2" data-yolo-preview>
            {!pv && !err && <div className="flex items-center gap-2 type-footnote text-muted-foreground"><Spinner className="size-3.5" />{t("yolo:switch.previewLoading")}</div>}
            {err && <div className="type-footnote text-destructive">{err}</div>}
            {pv && <>
              <div className="type-footnote text-muted-foreground"><Trans t={t} i18nKey={pv.hard_limited ? "yolo:switch.previewHard" : "yolo:switch.preview"} values={{ n, keep: pv.keep_for_you, hard: pv.hard_limited }} components={{ b: <b className="font-medium text-foreground" /> }} /></div>
              {n > 0 && <ul className="flex min-w-0 flex-col gap-1" data-yolo-would>
                {pv.would_handle.slice(0, 6).map((w) => <li key={w.decision_id} className="flex min-w-0 flex-wrap items-baseline gap-x-2 type-footnote"><span className="min-w-0 max-w-full">{w.question}</span><span className="min-w-0 max-w-full text-muted-foreground">{t("auto:quoted", { text: choiceText(w.choice, w.rule.startsWith("release") ? "release" : undefined) })}{w.delay_s ? t("yolo:switch.delay", { when: t("yolo:badge.inMinutes", { count: Math.round(w.delay_s / 60) }) }) : ""}</span></li>)}
              </ul>}
              {n > 0 && <label className="flex items-start gap-2 type-footnote"><Checkbox checked={all} onCheckedChange={(v) => setAll(!!v)} data-yolo-include-open /><span className="min-w-0">{t("yolo:switch.includeOpen", { count: n })}</span></label>}
            </>}
          </div>}
          {needsOk && <div className="anim-in flex min-w-0 shrink-0 flex-col gap-2 rounded-xl bg-attention/10 px-4 py-3" data-yolo-full-confirm>
            <div className="type-callout font-medium">{fullGives()}</div>
            <div className="type-footnote text-muted-foreground">{fullKeeps()}</div>
            <label className="mt-1 flex items-start gap-2 type-callout"><Checkbox className="mt-0.5" checked={ok} onCheckedChange={(v) => setOk(!!v)} data-yolo-full-ok /><span className="min-w-0">{t("yolo:switch.fullOk")}</span></label>
          </div>}
          </AutonomyDial>
          {s !== "forced" && !needsOk && <div className="type-footnote text-tertiary" data-yolo-never>{never()}</div>}
        </div>
        <DialogFooter className="min-w-0 gap-2 *:h-auto *:min-h-8 *:min-w-0 *:whitespace-normal">
          <Button variant="ghost" onClick={closeSheet}>{action ? t("common:action.cancel") : t("yolo:switch.done")}</Button>
          {action && <Button disabled={busy || (needsOk && !ok) || (turningOn && !pv && !err)} data-yolo-apply={sel}
            variant={sel === "full" ? "attention" : sel === "off" ? "outline" : "default"}
            onClick={() => void run(async () => { await yoloSet(sel, turningOn && all); closeSheet() })}>{action}</Button>}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
