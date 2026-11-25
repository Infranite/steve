import { VirtualList } from "@/components/virtual-list"
import { SessionIdentity } from "@/components/session-identity"
import { readViews, useEnded, useViewKinds } from "@/data/sync"
import { getItem, getSession, getSessions, sessOf, useShellStore } from "@/store"
import { HarnessMark } from "@/components/harness-mark"
import { api } from "@/api"
import { Button } from "@/components/ui/button"
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group"
import { t as tr, useT } from "@/i18n"
import { hm, monthDay, weekday } from "@/i18n/format"
import { SessionsHeatmap } from "@/sessions-heatmap"
import { SS, type Sess } from "@/store"
import { Empty, Group, ItemNo, openSheet, Page, SectionLabel, StatusDot, SearchField } from "@/ui"
import { Count, Select } from "@/view-shared"
import { cn } from "cn"
import { LockIcon, UploadIcon } from "lucide-react"
import { memo, useEffect, useRef, useState } from "react"

type SessFilt = "all" | "live" | "unlinked" | "imported"
/* kept across visits, like the items page's filters */
let sf: SessFilt = "all", sq = "", hf = "all"
const SFILTS: SessFilt[] = ["all", "live", "unlinked", "imported"]
const FILT_KEY = { all: "views:sessions.filterAll", live: "views:sessions.filterLive", unlinked: "views:sessions.filterUnlinked", imported: "views:sessions.filterImported" } as const
const live = (s: Sess) => s.st === "run" || s.st === "wait"
const inFilt = (k: SessFilt, s: Sess) => k === "all" || (k === "live" && live(s)) || (k === "unlinked" && !s.item) || (k === "imported" && !!s.ro)

const DAY = 86_400_000
const startOfDay = (ts: number) => { const d = new Date(ts); return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime() }
/** "今天", "昨天", "周二 · 11月10日" within the week, "11月3日" before it */
function dayLabel(ts: number, today: number) {
  const days = Math.round((today - startOfDay(ts)) / DAY)
  if (days <= 0) return tr("views:sessions.today")
  if (days === 1) return tr("views:sessions.yesterday")
  if (days < 7) return tr("views:sessions.weekdayDay", { weekday: weekday(ts), day: monthDay(ts) })
  return monthDay(ts)
}

/** context used: a short bar and the figure; amber from 80%, where a new turn starts over with a handoff */
function Ctx({ s }: { s: Sess }) {
  const t = useT()
  if (!s.contextKnown) return null
  const high = s.ctx >= 80
  return (
    <span className="inline-flex items-center gap-1.5" title={t("views:sessions.ctx", { pct: s.ctx })} data-session-ctx={s.ctx}>
      <span className="relative h-1 w-8 overflow-hidden rounded-full bg-foreground/10"><span className={cn("absolute inset-y-0 left-0 rounded-full", high ? "bg-attention" : s.st === "ended" ? "bg-tertiary" : "bg-muted-foreground")} style={{ width: Math.min(100, s.ctx) + "%" }} /></span>
      <span className={cn("w-7 text-right tabular-nums", high ? "text-attention" : "text-tertiary")}>{s.ctx}%</span>
    </span>
  )
}

const SessRow = memo(function SessRow({ s, when, hit, status }: { s: Sess; when: string; hit: string; status: Sess["st"] }) {
  const k = s.id
  const t = useT()
  const item = s.item ? getItem(s.item) : undefined
  /* the item's own title only when it says something the conversation's title does not */
  const itemTitle = item && item.t && !s.title.includes(item.t) && !item.t.includes(s.title) ? item.t : null
  return (
    <a href={"#/sessions/" + k} data-session-row={k} data-session-status={status}
      className="flex min-w-0 items-center gap-3 rounded-lg px-2 py-2 outline-none transition-colors duration-120 hover:bg-accent focus-visible:ring-[3px] focus-visible:ring-ring/50">
      <StatusDot s={s.st} />
      {/* the item column: its number, or that it has none */}
      <span className="w-12 shrink-0 truncate sm:w-[4.5rem]">{s.item ? <ItemNo id={s.item} /> : <span className="type-footnote text-tertiary" data-session-unlinked>{t("views:sessions.unlinked")}</span>}</span>
      <span className="flex min-w-0 flex-1 flex-col">
        <span className="flex min-w-0 items-baseline gap-2">
          <span className={cn("truncate type-body", live(s) ? "font-medium text-foreground" : s.st === "ended" ? "text-foreground/70" : "text-foreground")} title={s.title}>{s.title}</span>
          {s.n > 1 && <span className="shrink-0 type-footnote tabular-nums text-tertiary">#{s.n}</span>}
          {s.ro && <LockIcon className="size-3 shrink-0 self-center text-tertiary" aria-label={t("views:sessions.readOnly")} />}
          {itemTitle && <span className="hidden min-w-0 shrink-[2] truncate type-footnote text-muted-foreground md:block" data-session-item-title>{itemTitle}</span>}

        </span>
        <span className="mt-0.5 flex min-w-0 type-footnote text-muted-foreground" data-session-model><SessionIdentity session={s} /></span>
        {hit && <span className="mt-0.5 truncate type-footnote text-blue" data-session-hit>{t("views:sessions.contentHit", { q: hit })}</span>}
      </span>
      <span className="flex shrink-0 items-center gap-4 type-footnote text-muted-foreground">
        {live(s) && <span className={cn("whitespace-nowrap font-medium", s.st === "run" ? "text-blue" : "text-attention")}>{SS[s.st]?.[0]}</span>}
        <span className="hidden w-[4.25rem] sm:block"><Ctx s={s} /></span>
        <span className="whitespace-nowrap text-right sm:w-[4.5rem] tabular-nums text-tertiary">{when}</span>
      </span>
    </a>
  )
})

export function SessionsView() {
  const t = useT()
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const scroll = useRef<HTMLDivElement>(null)
  useViewKinds(["session", "decision", "item"])
  useShellStore()
  const history = useEnded(sf !== "live" && !sq)
  const [hits, setHits] = useState<{ q: string; ids: string[] } | null>(null)
  useEffect(() => {
    const q = sq
    if (!q) return
    let current = true
    const timer = setTimeout(() => void api.searchSessions(q).then(async (ids) => {
      if (!current) return
      await readViews("session", ids.filter((id) => !getSession(id)))
      if (current) setHits({ q, ids })
    }).catch(() => { if (current) setHits({ q, ids: [] }) }), 250)
    return () => { current = false; clearTimeout(timer) }
  }, [sq])

  const matches = new Set(hits?.q === sq ? hits.ids : [])
  const found = [...matches].flatMap((id) => getSession(id) ?? [])
  const all = [...new Map([...getSessions(), ...history.rows.map(sessOf), ...found].map((s) => [s.id, s] as const)).values()]
    .sort((a, b) => (b.at ?? 0) - (a.at ?? 0) || b.id.localeCompare(a.id))
  const searched = all.filter((s) => (hf === "all" || s.h === hf) && (!sq || s.title.includes(sq) || (s.item ?? "").toLowerCase().includes(sq.toLowerCase()) || matches.has(s.id)))
  const xs = searched.filter((s) => inFilt(sf, s))
  const counts = Object.fromEntries(SFILTS.map((k) => [k, searched.filter((s) => inFilt(k, s)).length])) as Record<SessFilt, number>
  const harnesses = [...new Set(all.flatMap((s) => s.h ?? []))]
  /* what is live first (waiting on you before running), then the rest by day */
  const liveRows = xs.filter(live).sort((a, b) => Number(b.st === "wait") - Number(a.st === "wait"))
  const rest = xs.filter((s) => !live(s))
  const today = startOfDay(Date.now())
  const days: [string, (typeof rest)[number][]][] = []
  for (const x of rest) { const l = dayLabel(x.at ?? 0, today); const last = days[days.length - 1]; if (last?.[0] === l) last[1].push(x); else days.push([l, [x]]) }

  const run = all.filter((s) => s.st === "run").length, wait = all.filter((s) => s.st === "wait").length
  const sub = [t("views:sessions.total", { count: all.length }), run > 0 && t("views:sessions.running", { count: run }), wait > 0 && t("views:sessions.waiting", { count: wait })].filter(Boolean).join(" · ")
  const row = (s: Sess, when: string) => <SessRow s={s} when={when} status={s.st} hit={sq && matches.has(s.id) && !s.title.includes(sq) ? sq : ""} />
  const filts = SFILTS.filter((k) => k === "all" || k === sf || counts[k] > 0)

  return (
    <Page virtual scrollRef={scroll} title={t("views:sessions.title")} sub={sub} max={1080}
      /* importing belongs to the list, on the title line, not at the window's far edge */
      toolbar={<Button size="sm" variant="outline" onClick={() => openSheet("import")} data-import><UploadIcon />{t("views:sessions.import")}</Button>} toolbarTop>
      <div className="mb-8 flex flex-col gap-3" data-sessions-toolbar>
        <SearchField aria-label={t("views:sessions.search")} placeholder={t("views:sessions.search")} defaultValue={sq} onChange={(e) => { sq = e.target.value.trim(); r() }} data-session-search />
        <div className="flex items-center gap-1 max-sm:-mx-4 max-sm:overflow-x-auto max-sm:px-4 max-sm:[scrollbar-width:none] [&>*]:shrink-0">
          <ToggleGroup value={[sf]} onValueChange={(v: any[]) => { if (v.length) { sf = v[v.length - 1] as SessFilt; r() } }} size="sm" spacing={1} data-session-filters>
            {filts.map((k) => (
              <ToggleGroupItem key={k} value={k} data-seg={k} className="h-7 rounded-lg px-2.5 type-callout text-muted-foreground hover:bg-accent hover:text-foreground data-pressed:bg-accent data-pressed:font-medium data-pressed:text-foreground">
                {t(FILT_KEY[k])}<span className={cn("ml-1.5 font-normal tabular-nums", k === "live" && counts.live > 0 ? "text-blue" : "text-tertiary")}>{counts[k]}</span>
              </ToggleGroupItem>
            ))}
          </ToggleGroup>
          <span className="flex-1" />
          {(harnesses.length > 1 || hf !== "all") && <Select quiet className={cn("h-7 px-2 type-callout [&>svg]:opacity-60", hf !== "all" ? "bg-accent font-medium! text-foreground" : "text-muted-foreground")} testId="harness-filter" value={hf} onChange={(v) => { hf = v; r() }} options={[{ v: "all", l: t("views:sessions.allHarness") }, ...harnesses.map((x) => ({ v: x, l: <span className="inline-flex items-center gap-1.5"><HarnessMark harness={x} className="size-3.5" />{x}</span> }))]} />}
        </div>
      </div>
      <SessionsHeatmap />
      {xs.length ? (
        <div className="flex flex-col gap-8">
          {liveRows.length > 0 && (
            <section data-sessions-group="live">
              <SectionLabel className="mb-1"><span>{t("views:sessions.groupLive")}<Count n={liveRows.length} /></span></SectionLabel>
              <Group><VirtualList rows={liveRows} render={(s) => row(s, s.up)} scroll={scroll} estimate={62} /></Group>
            </section>
          )}
          {days.map(([day, g]) => (
            <section key={day} data-sessions-group={day}>
              <SectionLabel className="mb-1"><span>{day}<Count n={g.length} /></span></SectionLabel>
              <Group><VirtualList rows={g} render={(s) => row(s, s.at ? hm(s.at) : s.up)} scroll={scroll} estimate={62} /></Group>
            </section>
          ))}

        </div>
      ) : <Empty title={all.length ? t("views:sessions.emptyFiltered") : t("views:sessions.emptyNone")} sub={all.length ? undefined : t("views:sessions.emptyNoneSub")} />}
      {sf !== "live" && !sq && history.more && <Button variant="ghost" size="sm" onClick={() => void history.load()} data-sessions-more>{t("common:action.more")}</Button>}
    </Page>
  )
}
