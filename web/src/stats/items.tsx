/* 事项表（stats.md §6.3 第 8 条）：事项号、标题、状态点、前置、Agent、等你、返工、tokens、估算费用；
 * 默认按费用降序，50 行一页；点一行进下钻 #/stats/items/:id。 */
import { useState } from "react"
import { cn } from "cn"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { stats, type Filter, type ItemSort } from "@/api/stats"
import { go, ItemNo, StatusDot } from "@/ui"
import { Select } from "@/views"
import { Est } from "@/stats/kpi"
import { Table, num, th } from "@/stats/tables"
import { fmtMs, fmtN, fmtUsd, useStats } from "@/stats/format"

const SORTS: [ItemSort, string][] = [["est", "按估算费用"], ["tokens", "按 tokens"], ["lead", "按前置时间"], ["wait", "按等你的时间"], ["reworks", "按返工次数"], ["done_at", "按完成时间"]]

export function ItemsTable({ f, fkey, live }: { f: Filter; fkey: string; live: boolean }) {
  const [sort, setSort] = useState<ItemSort>("est")
  const [cursor, setCursor] = useState<string[]>([])
  const at = cursor[cursor.length - 1]
  const [d] = useStats(() => stats.items(f, sort, at), [fkey, sort, at], live)
  return (
    <div data-items-table>
      <div className="mb-2 flex items-center gap-2">
        <Select value={sort} onChange={(v) => { setSort(v as ItemSort); setCursor([]) }} options={SORTS.map(([v, l]) => ({ v, l }))} testId="items-sort" />
        {d.s === "ok" && <span className="type-footnote text-muted-foreground">共 {d.v.total} 件（区间内完成或有活动）</span>}
      </div>
      {d.s === "loading" ? <div className="flex h-24 items-center justify-center"><Spinner className="size-4 text-muted-foreground" /></div>
        : d.s !== "ok" ? <div className="py-6 type-callout text-muted-foreground" role="alert">{d.s === "error" ? d.msg : "读不到事项"}</div>
        : !d.v.rows.length ? <div className="py-6 type-callout text-muted-foreground">这段时间没有事项在动。</div>
        : <>
          <Table label="事项" min={760} head={<tr><th className={cn(th, "w-20")}>事项</th><th className={th}>标题</th><th className={cn(th, "pl-4 text-right")}>前置</th><th className={cn(th, "pl-4 text-right")}>Agent</th><th className={cn(th, "pl-4 text-right")}>等你</th><th className={cn(th, "pl-4 text-right")}>返工</th><th className={cn(th, "pl-4 text-right")}>tokens</th><th className={cn(th, "pl-4 text-right")}><span className="inline-flex items-center gap-1">估算费用<Est /></span></th></tr>}>
            {d.v.rows.map((r) => (
              <tr key={r.item_id} tabIndex={0} className="cursor-pointer hover:bg-accent focus-visible:bg-accent focus-visible:outline-none" onClick={() => go("#/stats/items/" + r.item_id)} onKeyDown={(e) => e.key === "Enter" && go("#/stats/items/" + r.item_id)} data-stats-item={r.item_id}>
                <td className="py-2"><ItemNo id={r.item_id} /></td>
                <td className="max-w-72 py-2"><span className="flex min-w-0 items-center gap-2"><StatusDot s={r.status} /><span className="truncate">{r.title}</span></span></td>
                <td className={num}>{fmtMs(r.lead_ms)}</td><td className={num}>{fmtMs(r.agent_ms)}</td><td className={num}>{fmtMs(r.wait_ms)}</td>
                <td className={num}>{r.reworks}</td><td className={num}>{fmtN(r.tokens)}</td><td className={num}>{r.tokens ? fmtUsd(r.est) : r.reported ? <span title="只有会话级的适配器报价（下限）">≥{fmtUsd(r.reported)}</span> : "—"}</td>
              </tr>
            ))}
          </Table>
          {(cursor.length > 0 || d.v.next) && (
            <div className="mt-2 flex items-center justify-end gap-2">
              <Button size="xs" variant="ghost" disabled={!cursor.length} onClick={() => setCursor(cursor.slice(0, -1))}>上一页</Button>
              <Button size="xs" variant="ghost" disabled={!d.v.next} onClick={() => d.v.next && setCursor([...cursor, d.v.next])} data-items-next>下一页</Button>
            </div>
          )}
        </>}
    </div>
  )
}
