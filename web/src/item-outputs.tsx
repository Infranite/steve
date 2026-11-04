import type { RAnnotation } from "@/api"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { groupOuts, isImageOut, type OutV } from "@/out-groups"
import { OutPreview } from "@/outputs"
import { tm, type Out } from "@/store"
import { Empty, Group, Row } from "@/ui"
import { cn } from "cn"
import { ChevronRightIcon, HistoryIcon } from "lucide-react"

/* the 产出 tab of an item: what the latest delivery handed over; the work record keeps the records of the work and what
 * earlier deliveries handed over, by delivery, each still openable. And the dialog an output opens in. */

export type OutOpen = { o: Out; focus?: string }

function OutRowV({ x, anns, open, n }: { x: OutV<Out>; anns: RAnnotation[]; open: (o: Out) => void; n: string }) {
  const o = x.o, mine = anns.filter((a) => a.artifact_id === o[3]), drafts = mine.filter((a) => a.status !== "sent").length
  return (
    <Row title={<span className="break-words font-mono text-[13px]">{o[1]}</span>} sub={o[2] ?? undefined} onClick={() => open(o)} data-out={n} data-out-id={o[3]}>
      <span className="flex shrink-0 items-center gap-1.5 type-footnote text-muted-foreground">
        {drafts > 0 && <span className="rounded-full bg-attention/12 px-1.5 text-attention" data-out-drafts>{drafts} 条草稿</span>}
        {mine.length - drafts > 0 && <span data-out-sent>{mine.length - drafts} 条批注</span>}
        {x.record ? <span className="rounded bg-muted px-1.5">过程记录</span> : x.old ? <span className="rounded bg-muted px-1.5">{x.v < x.vs ? "旧版本" : "早先交付"}</span> : null}
        {x.vs > 1 && <span className="tabular-nums" data-out-ver>v{x.v}</span>}
        <span>{o[0]}</span>
      </span>
      <ChevronRightIcon className="size-4 text-tertiary" />
    </Row>
  )
}

export function OutList({ outs, anns, open }: { outs: Out[]; anns: RAnnotation[]; open: (o: Out) => void }) {
  if (!outs.length) return <Empty title="还没有产出" sub="Bot 交付时附上的分支、提交、文件和文档会出现在这里。" />
  const g = groupOuts(outs), count = g.records.reduce((n, r) => n + r.outs.length, 0)
  return (
    <div className="flex flex-col gap-4" data-outs>
      {g.final.length ? <div data-outs-final><Group>{g.final.map((x, n) => <OutRowV key={x.o[3]} x={x} anns={anns} open={open} n={String(n)} />)}</Group></div>
        : <div className="py-6 text-center type-callout text-muted-foreground" data-outs-none>最终交付还没有，过程记录在下面</div>}
      {count > 0 && <Collapsible defaultOpen={!g.final.length} data-out-records>
        <CollapsibleTrigger className="group/rec flex items-center gap-1.5 type-footnote text-muted-foreground hover:text-foreground" data-out-records-toggle>
          <ChevronRightIcon className="size-3.5 transition-transform group-data-[panel-open]/rec:rotate-90" /><HistoryIcon className="size-3.5" />执行记录 · {count} 项<span className="text-tertiary">过程记录和早先交付的版本</span>
        </CollapsibleTrigger>
        <CollapsibleContent className="mt-2 flex flex-col gap-3 pl-5">
          {g.records.map((r) => <div key={r.delivery} data-out-delivery={r.delivery}>
            <div className="type-footnote text-muted-foreground">第 {r.delivery} 次交付{r.at ? ` · ${tm(r.at)}` : ""}</div>
            <Group>{r.outs.map((x) => <OutRowV key={x.o[3]} x={x} anns={anns} open={open} n={`r${r.delivery}-${x.o[3]}`} />)}</Group>
          </div>)}
        </CollapsibleContent>
      </Collapsible>}
    </div>
  )
}

/** an output in a dialog; the pictures delivered with it are its neighbours */
export function OutDialog({ item, outs, anns, view, setView }: { item: string; outs: Out[]; anns: RAnnotation[]; view: OutOpen | null; setView: (v: OutOpen | null) => void }) {
  const g = groupOuts(outs)
  const all = [...g.final, ...g.records.flatMap((r) => r.outs)]
  const x = view && all.find((y) => y.o[3] === view.o[3])
  /* neighbours: the pictures of the same group (the outputs, or the same delivery in the record) */
  const group = !x ? [] : g.final.includes(x) ? g.final : g.records.find((r) => r.outs.includes(x))?.outs ?? []
  const siblings = group.map((y) => y.o).filter(isImageOut)
  return (
    <Dialog open={!!view} onOpenChange={(o) => !o && setView(null)}>
      <DialogContent className="flex max-h-[92dvh] flex-col gap-3 rounded-2xl max-sm:max-w-[calc(100%-1rem)] max-sm:p-4 sm:max-w-[960px]" data-preview>
        <DialogHeader><DialogTitle className="type-headline break-words pr-6">{view?.o[1]}</DialogTitle>
          <DialogDescription className="type-callout">{view?.o[0]} · 来自 {item}{x && x.vs > 1 && <span className={cn(x.v < x.vs && "text-attention")} data-preview-ver> · 第 {x.v} 版，共 {x.vs} 版{x.v < x.vs ? "（不是最新）" : ""}</span>}{x?.record && " · 过程记录"}</DialogDescription></DialogHeader>
        {view && <OutPreview key={view.o[3]} o={view.o} item={item} anns={anns} focus={view.focus} siblings={siblings} outs={outs} onOpen={(o, focus) => setView({ o, focus })} />}
      </DialogContent>
    </Dialog>
  )
}
