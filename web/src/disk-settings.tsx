import { useEffect, useState } from "react"
import { HubError, modReq } from "@/api"
import { Button } from "@/components/ui/button"
import { Progress } from "@/components/ui/progress"
import { Spinner } from "@/components/ui/spinner"
import { ItemNo, Row } from "@/ui"
import { Panel, Section, Note, Blank } from "@/settings-kit"
import { toast } from "@/store"
import { cn } from "cn"
import { useT } from "@/i18n"
import { stateLabel, bytes, diskRows, entryState, pressureText, reclaimText, scratchRows, type DiskEntry, type DiskReport, type Reclaimed } from "@/disk"

/** Settings · 数据 · 工作区与磁盘: what each item's worktree takes, how much of it is rebuildable cache, the disk
 * against Steve's thresholds, and a reclaim now. The Hub answers the instance owner only (paths of this machine);
 * anyone else sees nothing here. */
export function WorkspaceDisk() {
  const [r, setR] = useState<DiskReport | null>(null)
  const [err, setErr] = useState<string | null>(null)
  const [hidden, setHidden] = useState(false)
  const [busy, setBusy] = useState(false), t = useT()
  const load = () => {
    setErr(null)
    return modReq<DiskReport>("GET", "/system/workspaces", undefined, AbortSignal.timeout(120_000))
      .then(setR)
      .catch((e) => { if (e instanceof HubError && e.status === 403) setHidden(true); else setErr(e instanceof Error ? e.message : String(e)) })
  }
  useEffect(() => { void load() }, [])
  if (hidden) return null
  const reclaim = async () => {
    setBusy(true)
    try {
      const x = await modReq<Reclaimed>("POST", "/system/workspaces/reclaim", {}, AbortSignal.timeout(600_000))
      toast(reclaimText(x))
      await load()
    } catch (e) { toast(e instanceof Error ? e.message : String(e)) }
    setBusy(false)
  }
  const d = r && diskRows(r), sc = r && scratchRows(r)
  const entryRow = (e: DiskEntry, scratch: boolean) => {
    const s = entryState(e)
    return <Row key={e.item} data-disk-entry={e.item} data-disk-state={s} data-disk-scratch={scratch || undefined} onClick={() => { location.hash = "#/items/" + e.item }}
      title={<span className="inline-flex min-w-0 items-center gap-2"><ItemNo id={e.item} /><span className="truncate">{e.title}</span></span>}
      sub={<>{stateLabel(s)}{e.caches.length > 0 && <> · {t("shell:disk.caches", { names: e.caches.join(t("shell:list.sep")), bytes: bytes(e.cache_bytes) })}</>}</>}>
      <span className="type-callout tabular-nums text-muted-foreground">{bytes(e.bytes)}</span>
    </Row>
  }
  const used = r?.free != null && r.total ? 100 - (r.free / r.total) * 100 : null
  return <Section data-workspace-disk label={t("shell:disk.title")} right={<Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy || !r} onClick={reclaim} data-disk-reclaim>{busy ? t("shell:disk.reclaiming") : t("shell:disk.reclaim")}</Button>}>
    {!r && !err && <Blank className="flex items-center gap-2" data-disk-loading><Spinner className="size-3.5" />{t("shell:disk.measuring")}</Blank>}
    {err && <Blank>{t("shell:disk.measureFailed", { error: err })}<Button size="xs" variant="ghost" className="ml-2" onClick={() => void load()}>{t("common:action.retry")}</Button></Blank>}
    {r && d && <div className="flex flex-col gap-3 anim-in">
      <Panel>
        <Row title={t("shell:disk.disk")} sub={<span data-disk-pressure={r.pressure ?? "unknown"}>{pressureText(r)}</span>}>
          {used !== null && <Progress value={used} aria-label={t("shell:disk.used")} className={cn("w-16 sm:w-24 [&_[data-slot=progress-track]]:h-1.5", r.pressure === "floor" ? "[&_[data-slot=progress-indicator]]:bg-destructive" : "[&_[data-slot=progress-indicator]]:bg-foreground/60", "[&_[data-slot=progress-indicator]]:transition-[width] [&_[data-slot=progress-indicator]]:duration-260 [&_[data-slot=progress-indicator]]:ease-(--ease-out)")} />}
        </Row>
        <Row title={t("shell:disk.worktrees")} sub={d.reclaimable > 0 ? t("shell:disk.worktreesSubNext", { count: d.rows.length, cache: bytes(d.cache), next: bytes(d.reclaimable) }) : t("shell:disk.worktreesSub", { count: d.rows.length, cache: bytes(d.cache) })}>
          <span className="type-callout tabular-nums text-muted-foreground" data-disk-total>{bytes(d.total)}</span>
        </Row>
        {sc && sc.rows.length > 0 && <Row title={t("shell:disk.scratch")} sub={sc.ended > 0 ? t("shell:disk.scratchSubEnded", { count: sc.rows.length, cache: bytes(sc.cache), ended: bytes(sc.ended) }) : t("shell:disk.scratchSub", { count: sc.rows.length, cache: bytes(sc.cache) })} data-disk-scratch-total>
          <span className="type-callout tabular-nums text-muted-foreground">{bytes(sc.total)}</span>
        </Row>}
        {(r.orphans.length > 0 || r.trash > 0) && <Row title={t("shell:disk.other")} sub={[r.orphans.length ? t("shell:disk.orphans", { count: r.orphans.length }) : "", r.trash ? t("shell:disk.trash", { bytes: bytes(r.trash) }) : ""].filter(Boolean).join(t("shell:queue.sep"))}>
          <span className="type-callout tabular-nums text-muted-foreground">{bytes(r.orphans.reduce((s, o) => s + o.bytes, 0) + r.trash)}</span>
        </Row>}
      </Panel>
      {d.rows.length > 0 && <Panel>{d.rows.map((e) => entryRow(e, false))}</Panel>}
      {sc && sc.rows.length > 0 && <div data-disk-scratch-list><Panel>{sc.rows.map((e) => entryRow(e, true))}</Panel></div>}
      <Note className="mt-0">{t("shell:disk.policy")}{sc && sc.rows.length > 0 && t("shell:disk.scratchPolicy")}{d.gone > 0 && t("shell:disk.gone", { count: d.gone })}{t("shell:disk.git", { git: r.git ?? t("shell:disk.gitMissing") })}</Note>
    </div>}
  </Section>
}
