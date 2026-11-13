import { useState } from "react"
import { ChevronRightIcon, RotateCcwIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { useT } from "@/i18n"
import { dayHeading, hm } from "@/i18n/format"
import { cn } from "@/lib/utils"
import { st, undoDecision, WT, type Done } from "@/store"
import { clip } from "@/thread/topics"
import { Chips, Empty, go, Group, ItemNo, permissionQuestion, RelNote, SectionLabel } from "@/ui"
import { choiceText, YoloMark } from "@/yolo/badges"

/* 已处理: the decisions of the last 7 days, read back to check what was decided and by whom. A day holds one row per
 * item, the latest outcome first, so an item that stalled ten times is one line, not ten; the row opens to every
 * decision with its question and, while it can still be taken back, an undo. Decisions that lapsed on their own
 * (the item started again, was dropped) are their own filter: they needed nothing from you. */

type Who = "you" | "steve" | "lapsed"
const whoOf = (d: Done): Who => (d.how === "expired" ? "lapsed" : d.yolo !== undefined ? "steve" : "you")
type Entry = { d: Done; n: number; who: Who }

let filter: "all" | Who = "all"

function Outcome({ d, who, line }: { d: Done; who: Who; line?: boolean }) {
  const t = useT()
  if (d.rel) return <RelNote rel={d.rel} className="inline-flex" />
  const text = who === "lapsed" ? t("views:inbox.expired", { choice: d.choice })
    : who === "steve" ? t("views:inbox.steveChose", { choice: choiceText(d.choice) })
    : d.how === "dismissed" ? t("views:inbox.dismissed") : t("views:inbox.chose", { choice: d.choice })
  return <span className={cn("inline-flex min-w-0 items-center gap-1", who === "lapsed" && "text-tertiary")}>{who === "steve" && <YoloMark />}<span className={line ? "min-w-0 truncate" : "min-w-0 [overflow-wrap:anywhere] line-clamp-3"}>{text}{d.undone ? ` · ${t("views:inbox.undone")}` : ""}</span></span>
}

function ItemDay({ id, xs }: { id: string; xs: Entry[] }) {
  const t = useT()
  const [open, setOpen] = useState(false)
  const last = xs[0], item = st.items.find((i) => i.id === id)
  const title = item?.t ?? clip(permissionQuestion(last.d.q), 60)
  const more = xs.length - 1
  return (
    <div role="listitem" data-done-row={id}>
      <button type="button" onClick={() => setOpen(!open)} aria-expanded={open}
        className="flex w-full min-w-0 items-start gap-3 rounded-lg px-2 py-2.5 text-left transition-colors duration-120 hover:bg-accent">
        <ChevronRightIcon className={cn("mt-1 size-3.5 shrink-0 text-tertiary transition-transform duration-150", open && "rotate-90")} />
        <span className="flex min-w-0 flex-1 flex-col gap-0.5">
          <span className="flex min-w-0 items-baseline gap-2 type-body"><ItemNo id={id} /><span className="min-w-0 truncate">{title}</span></span>
          <span className="flex min-w-0 items-center gap-x-1 whitespace-nowrap type-footnote text-muted-foreground">
            <span className="shrink-0 text-tertiary">{WT[last.d.type]} ·</span><Outcome d={last.d} who={last.who} line />
            {more > 0 && <span className="shrink-0 text-tertiary">· {t("views:inbox.moreTimes", { count: more })}</span>}
          </span>
        </span>
        <span className="mt-0.5 shrink-0 type-footnote tabular-nums text-tertiary">{last.d.at ? hm(last.d.at) : last.d.tm}</span>
      </button>
      {open && <div className="anim-in mb-2 ml-[1.625rem] flex flex-col gap-3 border-l border-border pl-4 pt-1">
        {xs.map(({ d, n, who }) => (
          <div key={d.decision} className="flex min-w-0 items-start gap-3">
            <div className="flex min-w-0 flex-1 flex-col gap-0.5">
              <div className="type-callout text-foreground/85 [overflow-wrap:anywhere] line-clamp-3">{permissionQuestion(d.q)}</div>
              <div className="flex min-w-0 flex-wrap items-center gap-x-1 type-footnote text-muted-foreground">
                <Outcome d={d} who={who} />
                {who === "steve" && d.yolo && <span className="min-w-0 text-tertiary">· {d.yolo}</span>}
                {d.via && <span className="text-tertiary">· {t("views:inbox.via", { client: d.via })}</span>}
                <span className="tabular-nums text-tertiary">· {d.at ? hm(d.at) : d.tm}</span>
              </div>
            </div>
            {d.canUndo && <Button size="xs" variant="ghost" className="shrink-0 text-muted-foreground" onClick={() => undoDecision(n)} data-undo-decision><RotateCcwIcon />{t("views:inbox.undo")}</Button>}
          </div>))}
        {item && <Button size="xs" variant="ghost" className="-ml-2 w-fit text-muted-foreground" onClick={() => go("#/items/" + id)}>{t("views:inbox.openItem")}<ChevronRightIcon /></Button>}
      </div>}
    </div>
  )
}

export function InboxDone() {
  const t = useT()
  const [, f] = useState(0)
  const all: Entry[] = st.done.map((d, n) => ({ d, n, who: whoOf(d) })).sort((a, b) => b.d.at - a.d.at)
  if (!all.length) return <Empty title={t("views:inbox.doneEmpty")} />
  const count = (w: Who) => all.filter((x) => x.who === w).length
  const keys = (["all", "you", "steve", "lapsed"] as const).filter((k) => k === "all" || count(k) > 0)
  const label = (k: (typeof keys)[number]) => (k === "all" ? t("views:inbox.all") : t(`views:inbox.who.${k}`, { count: count(k) }))
  const cur = keys.includes(filter) ? filter : "all"
  const xs = all.filter((x) => cur === "all" || x.who === cur)
  /* by day, then by item within the day, in order of each item's latest decision */
  const days: [string, Map<string, Entry[]>][] = []
  for (const x of xs) {
    const day = x.d.at ? dayHeading(x.d.at) : ""
    let g = days[days.length - 1]
    if (!g || g[0] !== day) days.push((g = [day, new Map()]))
    const list = g[1].get(x.d.id)
    if (list) list.push(x); else g[1].set(x.d.id, [x])
  }
  return <>
    {keys.length > 2 && <Chips className="mb-6" value={label(cur)} onChange={(v) => { filter = keys.find((k) => label(k) === v) ?? "all"; f((n) => n + 1) }} options={keys.map(label)} />}
    <div className="flex flex-col gap-6">
      {days.map(([day, items]) => (
        <section key={day} className="anim-in">
          {day && <SectionLabel className="mb-1 text-tertiary">{day}</SectionLabel>}
          <Group>{[...items].map(([id, g]) => <ItemDay key={id} id={id} xs={g} />)}</Group>
        </section>))}
    </div>
    <div className="mt-4 type-footnote text-tertiary">{t("views:inbox.doneNote")}</div>
  </>
}
