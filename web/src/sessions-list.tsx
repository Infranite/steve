/* ═════════ 会话 (the list of every conversation) ═════════
 * Read as a log, newest first: what is live sits on top, the rest falls into days. One line per conversation with its
 * parts in fixed columns (item, title, model, context, time), so the eye runs down a column instead of parsing a line
 * of dots. What every listed conversation shares (one harness, one model, one bot, session #1) is not repeated on
 * each row. */
import { api } from "@/api"
import { Button } from "@/components/ui/button"
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group"
import { t as tr, useT } from "@/i18n"
import { hm, monthDay, weekday } from "@/i18n/format"
import { SessionsHeatmap } from "@/sessions-heatmap"
import { SS, st, type Sess } from "@/store"
import { BotAvatar, Empty, Group, ItemNo, openSheet, Page, SectionLabel, StatusDot, SearchField } from "@/ui"
import { Count, Select } from "@/view-shared"
import { cn } from "cn"
import { LockIcon, UploadIcon } from "lucide-react"
import { useEffect, useState } from "react"

type SessFilt = "all" | "live" | "unlinked" | "imported"
const PAGE = 60
/* kept across visits, like the items page's filters */
let sf: SessFilt = "all", sq = "", hf = "all", shown = PAGE
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

/* `model`/`bot`: whether the column is worth showing; `usual`: the harness, device and source most conversations share,
 * so a row names its own only when it is the exception */
type Cols = { model: boolean; bot: boolean; usual: { h?: string; device: string; src: string } }
const most = <T,>(xs: T[]) => { const n = new Map<T, number>(); let best: T | undefined, top = 0; for (const x of xs) { const c = (n.get(x) ?? 0) + 1; n.set(x, c); if (c > top) { top = c; best = x } } return best as T }
const deviceName = (k: string | undefined) => st.devices.find((d) => d.id === k)?.name ?? "Hub"

function SessRow({ k, s, cols, when, hit }: { k: string; s: Sess; cols: Cols; when: string; hit: boolean }) {
  const t = useT()
  const item = s.item ? st.items.find((x) => x.id === s.item) : undefined
  /* the item's own title only when it says something the conversation's title does not */
  const itemTitle = item && item.t && !s.title.includes(item.t) && !item.t.includes(s.title) ? item.t : null
  const ownHarness = s.h !== cols.usual.h && !!s.h && !s.src.toLowerCase().includes(s.h.toLowerCase())
  const where = [ownHarness && s.h, deviceName(s.device) !== cols.usual.device && deviceName(s.device), s.src !== cols.usual.src && s.src].filter(Boolean).join(" · ")
  return (
    <a href={"#/sessions/" + k} data-session-row={k} data-session-status={s.st}
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
          {where && <span className="min-w-0 shrink-[3] truncate type-footnote text-tertiary" data-session-where>{where}</span>}
        </span>
        {hit && <span className="mt-0.5 truncate type-footnote text-blue" data-session-hit>{t("views:sessions.contentHit", { q: sq })}</span>}
      </span>
      <span className="flex shrink-0 items-center gap-4 type-footnote text-muted-foreground">
        {live(s) && <span className={cn("whitespace-nowrap font-medium", s.st === "run" ? "text-blue" : "text-attention")}>{SS[s.st]?.[0]}</span>}
        {cols.bot && s.b && <span className="hidden sm:inline-flex" title={st.bots[s.b]?.n}><BotAvatar k={s.b} size={18} /></span>}
        {cols.model && <span className="hidden w-36 truncate md:block" title={[s.h, s.m].filter(Boolean).join(" · ")} data-session-model>{s.m}</span>}
        <span className="hidden w-[4.25rem] sm:block"><Ctx s={s} /></span>
        <span className="whitespace-nowrap text-right sm:w-[4.5rem] tabular-nums text-tertiary">{when}</span>
      </span>
    </a>
  )
}

export function SessionsView() {
  const t = useT()
  const [, f] = useState(0); const r = () => f((x) => x + 1)
  const [hits, setHits] = useState<Set<string> | null>(null)
  useEffect(() => { if (!sq) { setHits(null); return } const tm = setTimeout(() => api.searchSessions(sq).then((xs) => setHits(new Set(xs))).catch(() => setHits(new Set())), 250); return () => clearTimeout(tm) }, [sq])

  const all = Object.keys(st.S).map((k) => [k, st.S[k]] as const).sort((a, b) => (b[1].at ?? 0) - (a[1].at ?? 0))
  const matches = (k: string, s: Sess) => !sq || s.title.includes(sq) || (s.item ?? "").toLowerCase().includes(sq.toLowerCase()) || !!hits?.has(k)
  const searched = all.filter(([k, s]) => (hf === "all" || s.h === hf) && matches(k, s))
  const xs = searched.filter(([, s]) => inFilt(sf, s))
  const counts = Object.fromEntries(SFILTS.map((k) => [k, searched.filter(([, s]) => inFilt(k, s)).length])) as Record<SessFilt, number>

  /* a column or a part only when the listed conversations differ in it */
  const differ = (g: (s: Sess) => unknown) => new Set(xs.map(([, s]) => g(s))).size > 1
  const cols: Cols = { model: differ((s) => s.m), bot: differ((s) => s.b), usual: { h: most(all.map(([, s]) => s.h)), device: most(all.map(([, s]) => deviceName(s.device))), src: most(all.map(([, s]) => s.src)) } }
  const harnesses = [...new Set(all.map(([, s]) => s.h).filter(Boolean))] as string[]

  /* what is live first (waiting on you before running), then the rest by day */
  const liveRows = xs.filter(([, s]) => live(s)).sort((a, b) => (a[1].st === "wait" ? 0 : 1) - (b[1].st === "wait" ? 0 : 1))
  const rest = xs.filter(([, s]) => !live(s)), restShown = rest.slice(0, shown)
  const today = startOfDay(Date.now())
  const days: [string, (typeof rest)[number][]][] = []
  for (const x of restShown) { const l = dayLabel(x[1].at ?? 0, today); const last = days[days.length - 1]; if (last?.[0] === l) last[1].push(x); else days.push([l, [x]]) }

  const run = all.filter(([, s]) => s.st === "run").length, wait = all.filter(([, s]) => s.st === "wait").length
  const sub = [t("views:sessions.total", { count: all.length }), run > 0 && t("views:sessions.running", { count: run }), wait > 0 && t("views:sessions.waiting", { count: wait })].filter(Boolean).join(" · ")
  const row = ([k, s]: (typeof xs)[number], when: string) => <SessRow key={k} k={k} s={s} cols={cols} when={when} hit={!!sq && !!hits?.has(k) && !s.title.includes(sq)} />
  /* a filter with nothing in it is not offered, unless it is the one chosen */
  const filts = SFILTS.filter((k) => k === "all" || k === sf || counts[k] > 0)
  const reset = () => { shown = PAGE; r() }

  return (
    <Page title={t("views:sessions.title")} sub={sub} max={1080}
      /* importing belongs to the list, on the title line, not at the window's far edge */
      toolbar={<Button size="sm" variant="outline" onClick={() => openSheet("import")} data-import><UploadIcon />{t("views:sessions.import")}</Button>} toolbarTop>
      <div className="mb-8 flex flex-col gap-3" data-sessions-toolbar>
        <SearchField aria-label={t("views:sessions.search")} placeholder={t("views:sessions.search")} defaultValue={sq} onChange={(e) => { sq = e.target.value.trim(); reset() }} data-session-search />
        <div className="flex items-center gap-1 max-sm:-mx-4 max-sm:overflow-x-auto max-sm:px-4 max-sm:[scrollbar-width:none] [&>*]:shrink-0">
          <ToggleGroup value={[sf]} onValueChange={(v: any[]) => { if (v.length) { sf = v[v.length - 1] as SessFilt; reset() } }} size="sm" spacing={1} data-session-filters>
            {filts.map((k) => (
              <ToggleGroupItem key={k} value={k} data-seg={k} className="h-7 rounded-lg px-2.5 type-callout text-muted-foreground hover:bg-accent hover:text-foreground data-pressed:bg-accent data-pressed:font-medium data-pressed:text-foreground">
                {t(FILT_KEY[k])}<span className={cn("ml-1.5 font-normal tabular-nums", k === "live" && counts.live > 0 ? "text-blue" : "text-tertiary")}>{counts[k]}</span>
              </ToggleGroupItem>
            ))}
          </ToggleGroup>
          <span className="flex-1" />
          {(harnesses.length > 1 || hf !== "all") && <Select quiet className={cn("h-7 px-2 type-callout [&>svg]:opacity-60", hf !== "all" ? "bg-accent font-medium! text-foreground" : "text-muted-foreground")} testId="harness-filter" value={hf} onChange={(v) => { hf = v; reset() }} options={[{ v: "all", l: t("views:sessions.allHarness") }, ...harnesses.map((x) => ({ v: x, l: x }))]} />}
        </div>
      </div>
      <SessionsHeatmap />
      {xs.length ? (
        <div className="flex flex-col gap-8 anim-in">
          {liveRows.length > 0 && (
            <section data-sessions-group="live">
              <SectionLabel className="mb-1"><span>{t("views:sessions.groupLive")}<Count n={liveRows.length} /></span></SectionLabel>
              <Group>{liveRows.map((x) => row(x, x[1].up))}</Group>
            </section>
          )}
          {days.map(([day, g]) => (
            <section key={day} data-sessions-group={day}>
              <SectionLabel className="mb-1"><span>{day}<Count n={g.length} /></span></SectionLabel>
              <Group>{g.map((x) => row(x, x[1].at ? hm(x[1].at) : x[1].up))}</Group>
            </section>
          ))}
          {rest.length > restShown.length && (
            <Button variant="ghost" size="sm" className="self-start text-muted-foreground" onClick={() => { shown += PAGE; r() }} data-sessions-more>{t("views:sessions.more", { count: Math.min(PAGE, rest.length - restShown.length) })}</Button>
          )}
        </div>
      ) : <Empty title={all.length ? t("views:sessions.emptyFiltered") : t("views:sessions.emptyNone")} sub={all.length ? undefined : t("views:sessions.emptyNoneSub")} />}
    </Page>
  )
}
