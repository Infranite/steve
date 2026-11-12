/* What YOLO did or will do with a card (modules/yolo.md §8), kept small and grey: one line under a decision card —
 * “YOLO 替你选了「…」” once answered, a countdown with 现在 / 我来定 in the cool-down, “YOLO 没接：<原因>” when it left
 * the card to you — plus the item's and the project's own setting (跟随全局 / 关 / YOLO / 完全放开) and the mark on
 * YOLO's timeline lines. */
import { useCallback, useEffect, useState } from "react"
import { ChevronDownIcon, ZapIcon } from "lucide-react"
import { cn } from "cn"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuLabel, DropdownMenuRadioGroup, DropdownMenuRadioItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { yoloApi, type PermMode, type RYoloItem, type YoloMode } from "@/api/yolo"
import { st, useHubEvents, yoloCancel, yoloNow, yoloScope, type Item } from "@/store"
import { modeLines } from "@/yolo/modes"
import { Dot, fullGives, fullKeeps, levelOf, type YoloState } from "@/yolo/switch"
import { t, useT } from "@/i18n"

/** how a choice reads on the card */
const CHOICE = {
  pass: () => t("yolo:choice.pass"), rework: () => t("yolo:choice.rework"), yes: () => t("yolo:choice.yes"), continue: () => t("yolo:choice.continue"),
  retry: () => t("yolo:choice.retry"), send: () => t("yolo:choice.send"), once: () => t("yolo:choice.once"), approved: () => t("yolo:choice.once"), allow_once: () => t("yolo:choice.once"),
} as Record<string, () => string>
/* a tool-call card's option ids are the agent's own (`allow_once`, `allow`, …); YOLO only ever picks the allow-once one */
export const choiceText = (c: string | null | undefined, subject?: string) =>
  !c ? "" : subject === "release" && c === "yes" ? t("yolo:choice.releaseYes") : subject === "tool_call" ? t("yolo:choice.once") : Object.hasOwn(CHOICE, c) ? CHOICE[c]() : c

/** the small grey “⚡ YOLO” in front of what YOLO did */
export function YoloMark({ className }: { className?: string }) {
  return <span className={cn("inline-flex shrink-0 items-center gap-0.5 type-label text-muted-foreground", className)} data-yolo-mark><ZapIcon className="size-3" strokeWidth={2} aria-hidden />YOLO</span>
}

function useTick(on: boolean) {
  const [, f] = useState(0)
  useEffect(() => { if (!on) return; const t = setInterval(() => f((x) => x + 1), 1000); return () => clearInterval(t) }, [on])
}

const left = (ms: number) => {
  const s = Math.max(0, Math.round(ms / 1000))
  return s >= 60 ? t("yolo:badge.inMinutes", { count: Math.ceil(s / 60) }) : t("yolo:badge.inSeconds", { count: s })
}

const LINE = "col-span-full flex min-w-0 items-baseline gap-1.5 type-footnote text-muted-foreground"

/** under a decision card: what YOLO did, will do, or left to you */
export function YoloBadge({ decision, result, subject }: { decision: string; result?: any; subject?: string }) {
  const t = useT()
  const a = st.yoloActs[decision]
  /* forced off or paused, nothing scheduled will run: no countdown */
  const scheduled = a?.outcome === "scheduled" && a.due_at != null && !!st.yolo?.active
  useTick(scheduled)
  const [busy, setBusy] = useState(false)
  if (result?.decided_by === "yolo") return (
    <div className={LINE} data-yolo-badge="decided" title={result.yolo_reason || undefined}>
      <YoloMark /><span className="min-w-0 truncate">{((choice: string, reason: string | undefined) => choice
        ? (reason ? t("yolo:badge.choseWhy", { choice, reason }) : t("yolo:badge.chose", { choice }))
        : (reason ? t("yolo:badge.handledWhy", { reason }) : t("yolo:badge.handled")))(choiceText(result.choice ?? result.optionId ?? a?.choice, subject), result.yolo_reason || undefined)}</span></div>
  )
  if (!a) return null
  const btn = "shrink-0 font-medium text-foreground/80 hover:text-foreground disabled:opacity-50"
  if (scheduled) return (
    <div role="status" className={cn(LINE, "flex-wrap")} data-yolo-badge="scheduled">
      <YoloMark /><span className="min-w-0">{t("yolo:badge.willChoose", { when: left(a.due_at! - Date.now()), choice: choiceText(a.choice, subject) })}</span>
      <span className="flex-1" />
      <button type="button" className={cn(btn, "text-blue")} disabled={busy} onClick={() => { setBusy(true); void yoloNow(a.id).finally(() => setBusy(false)) }} data-yolo-now>{t("yolo:badge.now")}</button>
      <span className="text-tertiary">·</span>
      <button type="button" className={btn} disabled={busy} onClick={() => { setBusy(true); void yoloCancel(a.id).finally(() => setBusy(false)) }} data-yolo-cancel>{t("yolo:badge.mine")}</button>
    </div>
  )
  if (a.outcome === "declined") return (
    <div className={LINE} data-yolo-badge="declined" title={a.reason}>
      <YoloMark /><span className="line-clamp-2 min-w-0 break-words">{t("yolo:badge.declined", { reason: a.limit_what ?? a.reason })}</span></div>
  )
  if (a.outcome === "cancelled" && a.user_took_over) return (
    <div className={LINE} data-yolo-badge="yours"><YoloMark /><span>{t("yolo:badge.yours")}</span></div>
  )
  return null
}

/* ───────── scopes: the item's and the project's own setting ───────── */
const scopeOf = (type: "project" | "item", id: string) => st.yolo?.scopes.find((s) => s.object_type === type && s.object_id === id)

const levelText = (l: "off" | "on" | "full") => t(`yolo:level.${l}`)

/** what applies to this item now: item → project (a real directory defaults to off) → workspace */
export function yoloFor(i: Item): { on: boolean; level: "off" | "on" | "full"; state: YoloState; why: string } {
  const y = st.yolo
  const r = (level: "off" | "on" | "full", why: string, state?: YoloState) => ({ on: level !== "off", level, state: state ?? level, why })
  if (!y) return r("off", "")
  if (y.forced_off) return r("off", t("yolo:why.forced"), "forced")
  if (y.paused) return r("off", t("yolo:why.paused"), "paused")
  const lv = (m: YoloMode) => (m === "full" ? "full" : m === "on" ? "on" : "off")
  const it = scopeOf("item", i.id)
  if (it && it.mode !== "inherit") return r(lv(it.mode), it.reason || t("yolo:why.itemOwn"))
  if (i.project) {
    const p = scopeOf("project", i.project)
    if (p && p.mode !== "inherit") return r(lv(p.mode), p.default ? t("yolo:why.projectDefault") : t("yolo:why.followProject"))
  }
  return r(levelOf(y), t("yolo:why.followGlobal"))
}

/** the scope menu: 跟随 / 关 / YOLO / 完全放开 (full access asks first) */
function ScopeMenu({ type, id, cur, inherit, label, children }: { type: "project" | "item"; id: string; cur: YoloMode; inherit: string; label: string; children: React.ReactNode }) {
  const t = useT()
  const [ask, setAsk] = useState(false)
  const pick = (m: YoloMode) => { if (m === cur) return; if (m === "full") setAsk(true); else void yoloScope(type, id, m) }
  const opts: [YoloMode, string, string][] = [
    ["inherit", type === "item" ? t("yolo:scope.inheritItem") : t("yolo:scope.inheritProject"), inherit],
    ["off", t("common:state.off"), t("yolo:scope.offSub")],
    ["on", t("yolo:level.on"), t("yolo:scope.onSub")],
    ["full", t("yolo:level.full"), t("yolo:scope.fullSub")],
  ]
  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger disabled={!!st.yolo?.forced_off} render={<button type="button" className="inline-flex h-7 shrink-0 items-center gap-1.5 rounded-lg px-2 type-callout text-foreground/80 hover:bg-accent hover:text-foreground disabled:opacity-50" aria-label={label} data-yolo-scope={type} data-yolo-scope-mode={cur} />}>
          {children}<ChevronDownIcon className="size-3.5 text-muted-foreground" />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="w-[min(20rem,calc(100vw-2rem))] min-w-0 [overflow-wrap:anywhere]">
          <DropdownMenuGroup><DropdownMenuLabel className="type-label text-tertiary">{label}</DropdownMenuLabel></DropdownMenuGroup>
          <DropdownMenuSeparator />
          <DropdownMenuRadioGroup value={cur} onValueChange={(v) => pick(v as YoloMode)}>
            {opts.map(([m, title, sub]) => (
              <DropdownMenuRadioItem key={m} value={m} data-yolo-scope-opt={m} className="items-start">
                <span className="flex min-w-0 flex-col gap-0.5"><span className="type-callout font-medium">{title}</span><span className="type-footnote text-muted-foreground">{sub}</span></span>
              </DropdownMenuRadioItem>))}
          </DropdownMenuRadioGroup>
        </DropdownMenuContent>
      </DropdownMenu>
      <AlertDialog open={ask} onOpenChange={setAsk}>
        <AlertDialogContent className="max-h-[calc(100dvh-2rem)] w-[calc(100%-2rem)] grid-rows-[minmax(0,1fr)_auto] gap-4 p-4 [overflow-wrap:anywhere] sm:p-6" data-yolo-scope-confirm>
          <AlertDialogHeader className="min-h-0 min-w-0 overflow-y-auto p-1">
            <AlertDialogTitle>{type === "item" ? t("yolo:scope.confirmItem") : t("yolo:scope.confirmProject")}</AlertDialogTitle>
            <AlertDialogDescription className="flex flex-col gap-2"><span className="text-foreground">{fullGives()}</span><span>{fullKeeps()}</span></AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter className="min-w-0 *:h-auto *:min-h-8 *:min-w-0 *:whitespace-normal"><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel>
            <AlertDialogAction variant="attention" onClick={() => { setAsk(false); void yoloScope(type, id, "full") }} data-yolo-scope-full-ok>{t("yolo:level.full")}</AlertDialogAction></AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  )
}

/** the item page's line: what applies now and why, and the menu to change it for this item only */
export function YoloItemSwitch({ i }: { i: Item }) {
  const t = useT()
  if (!st.yolo) return null
  const cur = (scopeOf("item", i.id)?.mode ?? "inherit") as YoloMode
  const eff = yoloFor(i)
  const now = eff.state === "forced" ? t("yolo:scope.unavailable") : eff.state === "paused" ? t("yolo:why.paused") : levelText(eff.level)
  return (
    <div className="inline-flex min-w-0 flex-wrap items-center gap-x-1 type-footnote text-muted-foreground" data-yolo-item={eff.level}>
      <span className="pl-0.5">{t("yolo:scope.away")}</span>
      <ScopeMenu type="item" id={i.id} cur={cur} inherit={t("yolo:scope.inheritNow", { level: levelWord() })} label={t("yolo:scope.itemMenu")}>
        <Dot s={eff.state} /><span className="font-medium">{now}</span>
        {/* forced off or paused already says why in the word itself */}
        {eff.why && eff.state !== "forced" && eff.state !== "paused" && <span className="max-w-[28ch] truncate text-muted-foreground">{t("yolo:scope.why", { why: eff.why })}</span>}
      </ScopeMenu>
    </div>
  )
}

const modeName = (m: PermMode | null) => (m ? t(`yolo:mode.name.${m}`) : t("yolo:mode.none"))

/** under the item's switch: per session, the permission mode Steve wants next to the one the Node confirmed the
 *  harness runs in; a change on its way, waiting for the turn's end or refused says so, never “in effect” */
export function YoloItemModes({ i }: { i: Item }) {
  const t = useT()
  const [s, setS] = useState<RYoloItem | null>(null)
  const load = useCallback(() => { yoloApi.item(i.id).then(setS, () => setS(null)) }, [i.id])
  useEffect(() => { load() }, [load])
  const ids = new Set(s?.sessions.map((x) => x.session_id) ?? [])
  useHubEvents(["yolo", "yolo_action", "session", "decision", "event"], load, 400, {
    filter: (e) => e.type === "yolo" || e.type === "yolo_action" || (e.type === "event" ? ids.has(e.session_id) : "item_id" in e && e.item_id === i.id),
  })
  const lines = s ? modeLines(s.sessions) : []
  if (!lines.length) return null
  const tone = { ok: "text-success", wait: "text-attention", bad: "text-destructive" }
  return (
    <div className="-mt-3 mb-5 flex flex-col gap-0.5 pl-0.5 type-footnote text-muted-foreground" data-yolo-modes>
      {lines.map((l, n) => (
        <div key={n} className="flex min-w-0 flex-col" data-yolo-mode={l.key} title={t("yolo:mode.detail", { wanted: modeName(l.wanted), applied: modeName(l.applied) })}>
          <span className="min-w-0 break-words"><span className="font-medium text-foreground/80">{t("yolo:mode.label")}</span>{" "}
            <span className={tone[l.tone]}>{t(`yolo:mode.${l.key}`, { wanted: modeName(l.wanted), applied: modeName(l.applied), reason: l.reason })}</span></span>
          {l.residual && <span className="min-w-0 break-words" data-yolo-residual>{t("yolo:mode.residual")}</span>}
        </div>
      ))}
    </div>
  )
}

const levelWord = () => levelText(levelOf(st.yolo))

/** the project settings row's control (a real-directory project defaults to 关) */
export function YoloProjectSelect({ id, realDir }: { id: string; realDir: boolean }) {
  const t = useT()
  if (!st.yolo) return null
  const sc = scopeOf("project", id)
  /* a real-directory project without a setting counts as 关 (its agents change the real files) */
  const cur: YoloMode = sc?.default ? "off" : ((sc?.mode ?? (realDir ? "off" : "inherit")) as YoloMode)
  const word = cur === "inherit" ? t("yolo:scope.projectInherit", { level: levelWord() }) : levelText(cur === "full" ? "full" : cur === "on" ? "on" : "off")
  const s: YoloState = cur === "inherit" ? (levelOf(st.yolo) as YoloState) : cur === "full" ? "full" : cur === "on" ? "on" : "off"
  return (
    <span data-yolo-project={cur}>
      <ScopeMenu type="project" id={id} cur={cur} inherit={t("yolo:scope.inheritNow", { level: levelWord() })} label={t("yolo:scope.projectMenu")}><Dot s={s} /><span>{word}</span></ScopeMenu>
    </span>
  )
}
