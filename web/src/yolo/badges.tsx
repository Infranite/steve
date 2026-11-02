/* What YOLO did or will do with a card (modules/yolo.md §8), kept small and grey: one line under a decision card —
 * “YOLO 替你选了「…」” once answered, a countdown with 现在 / 我来定 in the cool-down, “YOLO 没接：<原因>” when it left
 * the card to you — plus the item's and the project's own setting (跟随全局 / 关 / YOLO / 完全放开) and the mark on
 * YOLO's timeline lines. */
import { useEffect, useState } from "react"
import { ChevronDownIcon, ZapIcon } from "lucide-react"
import { cn } from "cn"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuLabel, DropdownMenuRadioGroup, DropdownMenuRadioItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import type { YoloMode } from "@/api/yolo"
import { st, yoloCancel, yoloNow, yoloScope, type Item } from "@/store"
import { Dot, FULL_GIVES, FULL_KEEPS, levelOf, type YoloState } from "@/yolo/switch"

/** how a choice reads on the card */
const CHOICE: Record<string, string> = {
  pass: "验收通过", rework: "返工", yes: "批准", continue: "让它继续", retry: "重试", send: "发送", once: "允许这一次", approved: "允许这一次",
}
export const choiceText = (c: string | null | undefined, subject?: string) =>
  !c ? "" : subject === "release" && c === "yes" ? "批准发布" : CHOICE[c] ?? c

/** the small grey “⚡ YOLO” in front of what YOLO did */
export function YoloMark({ className }: { className?: string }) {
  return <span className={cn("inline-flex shrink-0 items-center gap-0.5 text-[11.5px] font-medium leading-4 text-muted-foreground", className)} data-yolo-mark><ZapIcon className="size-3" strokeWidth={2} aria-hidden />YOLO</span>
}

function useTick(on: boolean) {
  const [, f] = useState(0)
  useEffect(() => { if (!on) return; const t = setInterval(() => f((x) => x + 1), 1000); return () => clearInterval(t) }, [on])
}

const left = (ms: number) => {
  const s = Math.max(0, Math.round(ms / 1000))
  return s >= 60 ? `${Math.ceil(s / 60)} 分钟后` : `${s} 秒后`
}

const LINE = "col-span-full flex min-w-0 items-baseline gap-1.5 type-footnote text-muted-foreground"

/** under a decision card: what YOLO did, will do, or left to you */
export function YoloBadge({ decision, result, subject }: { decision: string; result?: any; subject?: string }) {
  const a = st.yoloActs[decision]
  /* forced off or paused, nothing scheduled will run: no countdown */
  const scheduled = a?.outcome === "scheduled" && a.due_at != null && !!st.yolo?.active
  useTick(scheduled)
  const [busy, setBusy] = useState(false)
  if (result?.decided_by === "yolo") return (
    <div className={LINE} data-yolo-badge="decided" title={result.yolo_reason || undefined}>
      <YoloMark /><span className="min-w-0 truncate">{(c => c ? `替你选了「${c}」` : "替你处理了")(choiceText(result.choice ?? result.optionId ?? a?.choice, subject))}{result.yolo_reason ? ` · ${result.yolo_reason}` : ""}</span></div>
  )
  if (!a) return null
  const btn = "shrink-0 font-medium text-foreground/80 hover:text-foreground disabled:opacity-50"
  if (scheduled) return (
    <div role="status" className={cn(LINE, "flex-wrap")} data-yolo-badge="scheduled">
      <YoloMark /><span className="min-w-0">{left(a.due_at! - Date.now())}会选「{choiceText(a.choice, subject)}」</span>
      <span className="flex-1" />
      <button type="button" className={cn(btn, "text-blue")} disabled={busy} onClick={() => { setBusy(true); void yoloNow(a.id).finally(() => setBusy(false)) }} data-yolo-now>现在</button>
      <span className="text-tertiary">·</span>
      <button type="button" className={btn} disabled={busy} onClick={() => { setBusy(true); void yoloCancel(a.id).finally(() => setBusy(false)) }} data-yolo-cancel>我来定</button>
    </div>
  )
  if (a.outcome === "declined") return (
    <div className={LINE} data-yolo-badge="declined" title={a.reason}>
      <YoloMark /><span className="line-clamp-2 min-w-0 break-words">没接：{a.reason.replace(/^硬限制：/, "")}</span></div>
  )
  if (a.outcome === "cancelled" && a.reason === "你选择自己来定") return (
    <div className={LINE} data-yolo-badge="yours"><YoloMark /><span>这张你来定</span></div>
  )
  return null
}

/* ───────── scopes: the item's and the project's own setting ───────── */
const scopeOf = (type: "project" | "item", id: string) => st.yolo?.scopes.find((s) => s.object_type === type && s.object_id === id)

const WORD: Record<"off" | "on" | "full", string> = { off: "每步等你", on: "YOLO 替你审批", full: "完全放开" }

/** what applies to this item now: item → project (a real directory defaults to off) → workspace */
export function yoloFor(i: Item): { on: boolean; level: "off" | "on" | "full"; state: YoloState; why: string } {
  const y = st.yolo
  const r = (level: "off" | "on" | "full", why: string, state?: YoloState) => ({ on: level !== "off", level, state: state ?? level, why })
  if (!y) return r("off", "")
  if (y.forced_off) return r("off", "这台 Hub 上不可用", "forced")
  if (y.paused) return r("off", "YOLO 已暂停", "paused")
  const lv = (m: YoloMode) => (m === "full" ? "full" : m === "on" ? "on" : "off")
  const it = scopeOf("item", i.id)
  if (it && it.mode !== "inherit") return r(lv(it.mode), it.reason || "这件单独设置")
  if (i.project) {
    const p = scopeOf("project", i.project)
    if (p && p.mode !== "inherit") return r(lv(p.mode), p.default ? "项目没有 worktree，默认关" : "跟随项目")
  }
  return r(levelOf(y), "跟随全局")
}

/** the scope menu: 跟随 / 关 / YOLO / 完全放开 (full access asks first) */
function ScopeMenu({ type, id, cur, inherit, label, children }: { type: "project" | "item"; id: string; cur: YoloMode; inherit: string; label: string; children: React.ReactNode }) {
  const [ask, setAsk] = useState(false)
  const pick = (m: YoloMode) => { if (m === cur) return; if (m === "full") setAsk(true); else void yoloScope(type, id, m) }
  const opts: [YoloMode, string, string][] = [
    ["inherit", "跟随" + (type === "item" ? "项目和全局" : "全局"), inherit],
    ["off", "关", "每个决定都等你"],
    ["on", "YOLO 替你审批", "能撤回的决定 Steve 来做"],
    ["full", "完全放开", "Agent 不再请求权限"],
  ]
  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger disabled={!!st.yolo?.forced_off} render={<button type="button" className="inline-flex h-7 shrink-0 items-center gap-1.5 rounded-md px-2 text-[12.5px] text-foreground/85 transition-colors hover:bg-muted disabled:opacity-50" aria-label={label} data-yolo-scope={type} data-yolo-scope-mode={cur} />}>
          {children}<ChevronDownIcon className="size-3.5 text-muted-foreground" />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="min-w-64">
          <DropdownMenuGroup><DropdownMenuLabel className="type-footnote text-muted-foreground">{label}</DropdownMenuLabel></DropdownMenuGroup>
          <DropdownMenuSeparator />
          <DropdownMenuRadioGroup value={cur} onValueChange={(v) => pick(v as YoloMode)}>
            {opts.map(([m, t, sub]) => (
              <DropdownMenuRadioItem key={m} value={m} data-yolo-scope-opt={m} className="items-start">
                <span className="flex min-w-0 flex-col"><span className="text-[13.5px]">{t}</span><span className="type-footnote text-muted-foreground">{sub}</span></span>
              </DropdownMenuRadioItem>))}
          </DropdownMenuRadioGroup>
        </DropdownMenuContent>
      </DropdownMenu>
      <AlertDialog open={ask} onOpenChange={setAsk}>
        <AlertDialogContent data-yolo-scope-confirm>
          <AlertDialogHeader>
            <AlertDialogTitle>{type === "item" ? "这件事项" : "这个项目的事项"}完全放开？</AlertDialogTitle>
            <AlertDialogDescription className="flex flex-col gap-2"><span className="text-foreground">{FULL_GIVES}</span><span>{FULL_KEEPS}</span></AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction className="bg-attention text-white hover:bg-attention/90" onClick={() => void yoloScope(type, id, "full")} data-yolo-scope-full-ok>完全放开</AlertDialogAction></AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  )
}

/** the item page's line: what applies now and why, and the menu to change it for this item only */
export function YoloItemSwitch({ i }: { i: Item }) {
  if (!st.yolo) return null
  const cur = (scopeOf("item", i.id)?.mode ?? "inherit") as YoloMode
  const eff = yoloFor(i)
  const now = eff.state === "forced" ? "YOLO 不可用" : eff.state === "paused" ? "YOLO 已暂停" : WORD[eff.level]
  return (
    <div className="flex flex-wrap items-center gap-x-1 type-footnote text-muted-foreground" data-yolo-item={eff.level}>
      <span className="pl-0.5">你不在时：</span>
      <ScopeMenu type="item" id={i.id} cur={cur} inherit={`现在是「${levelWord()}」`} label="这件事项你不在时">
        <Dot s={eff.state} /><span className="font-medium">{now}</span>
        {eff.why && <span className="max-w-[28ch] truncate text-muted-foreground">· {eff.why}</span>}
      </ScopeMenu>
    </div>
  )
}

const levelWord = () => WORD[levelOf(st.yolo)]

/** the project settings row's control (a real-directory project defaults to 关) */
export function YoloProjectSelect({ id, realDir }: { id: string; realDir: boolean }) {
  if (!st.yolo) return null
  const sc = scopeOf("project", id)
  /* a real-directory project without a setting counts as 关 (its agents change the real files) */
  const cur: YoloMode = sc?.default ? "off" : ((sc?.mode ?? (realDir ? "off" : "inherit")) as YoloMode)
  const word = cur === "inherit" ? `跟随全局（${levelWord()}）` : WORD[cur === "full" ? "full" : cur === "on" ? "on" : "off"]
  const s: YoloState = cur === "inherit" ? (levelOf(st.yolo) as YoloState) : cur === "full" ? "full" : cur === "on" ? "on" : "off"
  return (
    <span data-yolo-project={cur}>
      <ScopeMenu type="project" id={id} cur={cur} inherit={`现在是「${levelWord()}」`} label="这个项目的事项你不在时"><Dot s={s} /><span>{word}</span></ScopeMenu>
    </span>
  )
}
