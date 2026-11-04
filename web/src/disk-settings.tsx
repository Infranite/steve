import { useEffect, useState } from "react"
import { HubError, modReq } from "@/api"
import { Button } from "@/components/ui/button"
import { Progress } from "@/components/ui/progress"
import { Group, ItemNo, Row, SectionLabel } from "@/ui"
import { toast } from "@/store"
import { cn } from "cn"
import { STATE_LABEL, bytes, diskRows, entryState, pressureText, reclaimText, type DiskReport, type Reclaimed } from "@/disk"

/** Settings · 数据 · 工作区与磁盘: what each item's worktree takes, how much of it is rebuildable cache, the disk
 * against Steve's thresholds, and a reclaim now. The Hub answers the instance owner only (paths of this machine);
 * anyone else sees nothing here. */
export function WorkspaceDisk() {
  const [r, setR] = useState<DiskReport | null>(null)
  const [err, setErr] = useState<string | null>(null)
  const [hidden, setHidden] = useState(false)
  const [busy, setBusy] = useState(false)
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
  const d = r && diskRows(r)
  const used = r?.free != null && r.total ? 100 - (r.free / r.total) * 100 : null
  return <section data-workspace-disk>
    <SectionLabel right={<Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy || !r} onClick={reclaim} data-disk-reclaim>{busy ? "正在回收…" : "立即回收"}</Button>}>工作区与磁盘</SectionLabel>
    {!r && !err && <p className="type-callout text-muted-foreground" data-disk-loading>正在测量工作区占用…</p>}
    {err && <p className="type-callout text-muted-foreground">测量失败：{err}<Button size="xs" variant="ghost" className="ml-2" onClick={() => void load()}>重试</Button></p>}
    {r && d && <div className="flex flex-col gap-3">
      <Group>
        <Row title="磁盘" sub={<span data-disk-pressure={r.pressure ?? "unknown"}>{pressureText(r)}</span>}>
          {used !== null && <Progress value={used} aria-label="磁盘已用" className={cn("w-16 sm:w-24 [&_[data-slot=progress-track]]:h-1.5", r.pressure === "floor" ? "[&_[data-slot=progress-indicator]]:bg-destructive" : "[&_[data-slot=progress-indicator]]:bg-foreground/60")} />}
        </Row>
        <Row title="任务工作区" sub={`${d.rows.length} 个，可重建的缓存 ${bytes(d.cache)}${d.reclaimable > 0 ? `，其中 ${bytes(d.reclaimable)} 下一轮回收` : ""}`}>
          <span className="type-callout tabular-nums text-muted-foreground" data-disk-total>{bytes(d.total)}</span>
        </Row>
        {(r.orphans.length > 0 || r.trash > 0) && <Row title="其他目录" sub={[r.orphans.length ? `${r.orphans.length} 个没有事项对应的目录（不自动删除）` : "", r.trash ? `待删除 ${bytes(r.trash)}` : ""].filter(Boolean).join("；")}>
          <span className="type-callout tabular-nums text-muted-foreground">{bytes(r.orphans.reduce((s, o) => s + o.bytes, 0) + r.trash)}</span>
        </Row>}
      </Group>
      {d.rows.length > 0 && <Group>
        {d.rows.map((e) => {
          const s = entryState(e)
          return <Row key={e.item} data-disk-entry={e.item} data-disk-state={s} onClick={() => { location.hash = "#/items/" + e.item }}
            title={<span className="inline-flex min-w-0 items-center gap-2"><ItemNo id={e.item} /><span className="truncate">{e.title}</span></span>}
            sub={<>{STATE_LABEL[s]}{e.caches.length > 0 && <> · 缓存 {e.caches.join("、")} {bytes(e.cache_bytes)}</>}</>}>
            <span className="type-callout tabular-nums text-muted-foreground">{bytes(e.bytes)}</span>
          </Row>
        })}
      </Group>}
      <p className="type-footnote text-muted-foreground">事项结束后，Steve 回收它的构建缓存；分支已合入主线或事项已归档时再删除工作区，分支和提交始终保留。有未提交的改动、会话还在运行或有进程在用时不动。{d.gone > 0 && `已回收 ${d.gone} 个工作区，事项继续时从分支重建。`}Git {r.git ?? "未找到"} · 每 10 分钟检查一次。</p>
    </div>}
  </section>
}
