/* 事项表（stats.md §6.3 第 8 条）：事项号、标题、状态点、前置、Agent、等你、返工、tokens、估算费用；
 * 默认按费用降序，50 行一页；点一行进下钻 #/stats/items/:id。`only` 是概览的下钻（ST-40）：只列区间内完成或新建的那批。 */
import { useState, type ReactNode } from "react"
import { Panel } from "@/panel"
import { cn } from "cn"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { stats, type Filter, type ItemOnly, type ItemSort } from "@/api/stats"
import { go, ItemNo, StatusDot } from "@/ui"
import { Select } from "@/view-shared"
import { Est } from "@/stats/kpi"
import { Table, num, th } from "@/stats/tables"
import { t, useT } from "@/i18n"
import { fmtEst, fmtMs, fmtN, fmtUsd, useStats } from "@/stats/format"

const SORTS = (): [ItemSort, string][] => [["est", t("stats:items.sort.est")], ["tokens", t("stats:items.sort.tokens")], ["lead", t("stats:items.sort.lead")], ["wait", t("stats:items.sort.wait")], ["reworks", t("stats:items.sort.reworks")], ["done_at", t("stats:items.sort.doneAt")]]

/** `panel`: the table is a panel of its own, its sort in the head's right slot and its count under the title, as every
 *  panel places them; without it (inside a drill-down) the sort and count sit in a row above the table */
export function ItemsTable({ f, fkey, live, only, sort0 = "est", panel }: { f: Filter; fkey: string; live: boolean; only?: ItemOnly; sort0?: ItemSort; panel?: { k: string; icon: ReactNode; title: ReactNode; sub?: ReactNode } }) {
  const t = useT()
  const [sort, setSort] = useState<ItemSort>(sort0)
  const [cursor, setCursor] = useState<string[]>([])
  const at = cursor[cursor.length - 1]
  const [d] = useStats(() => stats.items(f, sort, at, 50, only), [fkey, sort, at, only], live)
  const sorter = <Select value={sort} onChange={(v) => { setSort(v as ItemSort); setCursor([]) }} options={SORTS().map(([v, l]) => ({ v, l }))} testId="items-sort" />
  const count = d.s === "ok" ? <span data-items-total={d.v.total}>{only === "done" ? t("stats:items.totalDone", { count: d.v.total }) : only === "created" ? t("stats:items.totalCreated", { count: d.v.total }) : t("stats:items.totalActive", { count: d.v.total })}</span> : null
  const body = (
    <div data-items-table className={panel ? "pb-3" : undefined}>
      {!panel && <div className="mb-2 flex items-center gap-2 px-5">{sorter}{count && <span className="type-footnote text-muted-foreground">{count}</span>}</div>}
      {d.s === "loading" ? <div className="flex h-24 items-center justify-center"><Spinner className="size-4 text-muted-foreground" /></div>
        : d.s !== "ok" ? <div className="px-5 py-6 type-callout text-muted-foreground" role="alert">{d.s === "error" ? d.msg : t("stats:items.loadFailed")}</div>
        : !d.v.rows.length ? <div className="px-5 py-6 type-callout text-muted-foreground">{only === "done" ? t("stats:items.emptyDone") : only === "created" ? t("stats:items.emptyCreated") : t("stats:items.emptyActive")}</div>
        : <>
          <Table label={t("stats:col.item")} min={760} head={<tr><th className={cn(th, "w-20 whitespace-nowrap pr-3")}>{t("stats:col.item")}</th><th className={th}>{t("stats:col.title")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.lead")}</th><th className={cn(th, "pl-4 text-right")}>Agent</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.wait")}</th><th className={cn(th, "pl-4 text-right")}>{t("stats:col.reworks")}</th><th className={cn(th, "pl-4 text-right")}>tokens</th><th className={cn(th, "pl-4 text-right")}><span className="inline-flex items-center gap-1">{t("stats:col.cost")}<Est /></span></th></tr>}>
            {d.v.rows.map((r) => (
              <tr key={r.item_id} tabIndex={0} className="cursor-pointer hover:bg-muted/60 focus-visible:bg-muted/60 focus-visible:outline-none" onClick={() => go("#/stats/items/" + r.item_id)} onKeyDown={(e) => e.key === "Enter" && go("#/stats/items/" + r.item_id)} data-stats-item={r.item_id}>
                <td className="whitespace-nowrap py-2.5 pr-3"><ItemNo id={r.item_id} /></td>
                <td className="max-w-72 py-2.5"><span className="flex min-w-0 items-center gap-2"><StatusDot s={r.status} /><span className="truncate text-foreground">{r.title}</span></span></td>
                <td className={num}>{fmtMs(r.lead_ms)}</td><td className={num}>{fmtMs(r.agent_ms)}</td><td className={num}>{fmtMs(r.wait_ms)}</td>
                <td className={num}>{r.reworks}</td><td className={num}>{fmtN(r.tokens)}</td><td className={num}>{r.tokens ? <span title={r.unpriced_tokens ? t("stats:word.estUnpricedTitle", { n: fmtN(r.unpriced_tokens) }) : undefined} data-unpriced={r.unpriced_tokens || undefined}>{fmtEst(r.est, r.unpriced_tokens)}</span> : r.reported ? <span title={t("stats:word.sessionQuoteLowerBound")}>≥{fmtUsd(r.reported)}</span> : "—"}</td>
              </tr>
            ))}
          </Table>
          {(cursor.length > 0 || d.v.next) && (
            <div className="mt-2 flex items-center justify-end gap-2 px-5">
              <Button size="xs" variant="ghost" disabled={!cursor.length} onClick={() => setCursor(cursor.slice(0, -1))}>{t("stats:items.prevPage")}</Button>
              <Button size="xs" variant="ghost" disabled={!d.v.next} onClick={() => d.v.next && setCursor([...cursor, d.v.next])} data-items-next>{t("stats:items.nextPage")}</Button>
            </div>
          )}
        </>}
    </div>
  )
  return panel ? <Panel k={panel.k} icon={panel.icon} title={panel.title} sub={<>{count}{count && panel.sub ? " · " : null}{panel.sub}</>} right={sorter}>{body}</Panel> : body
}
