/* 事件浏览器（统计页）：这段时间里每个 Agent 回合、每张决定卡、每个事项节点（交付、发布、状态、失败）。
 * 上面是事件直方图：在上面拖出一个时间窗口，窗口里的柱子亮起来，窗口上方浮出“时间段 · 多少个事件”和放大、缩小、清除；
 * 按住窗口中间可以左右滑动，拖两边改宽窄，键盘左右键滑动、Shift+左右键改宽、回车放大、Esc 清除。
 * 放大后直方图铺满这个窗口（可以一层层放大，缩小回到上一层）。出过错的时段在柱子上方标一个红框。
 * 下面的列表和数字条只看窗口里的事件，最新的在前。数据来自 GET /stats/events。 */
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent, type PointerEvent as RPointerEvent } from "react"
import { cn } from "cn"
import { RadarIcon, XIcon, ZoomInIcon, ZoomOutIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { stats, type EventKind, type Events, type Filter, type StatEvent } from "@/api/stats"
import { ST } from "@/store"
import { go, ItemNo, Segmented } from "@/ui"
import { Panel, PanelFoot, StatStrip } from "@/panel"
import { Loading } from "@/stats/overview"
import { DECISION, RELEASE, STOP } from "@/stats/drill"
import { fmtMs, fmtN, tickLabel, timeTicks, useStats } from "@/stats/format"
import { t, useT } from "@/i18n"

type Win = [number, number]
const KIND_COLOR: Record<EventKind, string> = { turn: "var(--blue)", decision: "var(--attention)", mark: "var(--tertiary)" }
const pad = (n: number) => String(n).padStart(2, "0")
/** a time as the axis and the window label write it: clock time within two days, else the date too */
const at = (ts: number, long: boolean, secs = false) => {
  const d = new Date(ts), hm = `${pad(d.getHours())}:${pad(d.getMinutes())}${secs ? ":" + pad(d.getSeconds()) : ""}`
  return long ? `${d.getMonth() + 1}/${d.getDate()} ${hm}` : hm
}
const FAIL = () => ({ stall: t("stats:quality.fail.stall"), error: t("stats:quality.fail.error"), restart: t("stats:quality.fail.restart"), release: t("stats:quality.fail.release"), other: t("stats:quality.fail.other") }) as Record<string, string>

/** what happened, in a few words */
export function eventLabel(e: StatEvent) {
  if (e.kind === "turn") return e.open ? t("stats:events.turnRunning") : t("stats:events.turn", { what: STOP()[e.what] ?? (e.what || "—") })
  if (e.kind === "decision") {
    const kind = DECISION()[e.what] ?? e.what
    const state = e.open ? t("stats:events.decOpen") : e.by === "yolo" ? t("stats:events.decYolo") : e.by === "me" ? t("stats:events.decMe") : t("stats:events.decExpired")
    return t("stats:events.decision", { kind, state })
  }
  if (e.what === "status") return t("stats:events.status", { to: e.to === "fail" ? t("stats:events.failed") : ST[e.to ?? ""]?.[0] ?? e.to ?? "—" })
  if (e.what === "deliver") return e.to === "auto" ? t("stats:drill.deliverAuto") : t("stats:drill.deliver")
  if (e.what === "release") return RELEASE()[e.to ?? ""] ?? t("stats:drill.releaseMark")
  if (e.what === "fail") return t("stats:events.fail", { why: FAIL()[e.to ?? ""] ?? e.to ?? "—" })
  return e.what
}

export function EventExplorer({ f, fkey, live }: { f: Filter; fkey: string; live: boolean }) {
  const t = useT()
  const [zoom, setZoom] = useState<Win[]>([])
  const [sel, setSel] = useState<Win | null>(null)
  const [kind, setKind] = useState<"all" | EventKind>("all")
  /* a new page filter starts over */
  useEffect(() => { setZoom([]); setSel(null) }, [fkey.split("#")[0]])
  const span = zoom.at(-1)
  const q = { lo: span?.[0], hi: span?.[1], a: sel?.[0], b: sel?.[1], kind: kind === "all" ? undefined : kind, buckets: typeof window !== "undefined" && window.innerWidth < 640 ? 48 : 96 }
  const [d] = useStats(() => stats.events(f, q), [fkey, JSON.stringify(q)], live)
  const v = d.s === "ok" && d.v.histogram ? d.v : null
  /* a Hub without the explorer: no panel */
  if (d.s === "absent") return null
  /* the date goes with the clock time once the span covers more than one day */
  const long = v ? new Date(v.lo).toDateString() !== new Date(v.hi - 1).toDateString() : false
  const zoomIn = (w: Win) => { setZoom([...zoom, w]); setSel(null) }
  return (
    <Panel k="events" className="@container/events" data-events icon={<RadarIcon />} title={t("stats:events.title")} sub={t("stats:events.sub")}
      right={<Segmented size="sm" value={kind} onChange={(k) => setKind(k)} options={[["all", t("stats:events.all")], ["turn", t("stats:events.turns")], ["decision", t("stats:events.decisions")], ["mark", t("stats:events.marks")]] as const} />}>
      <div className="flex min-h-10 flex-wrap items-center gap-1.5 px-5 pt-1 pb-1" data-events-crumbs>
        <span className="mr-0.5 type-label text-tertiary">{t("stats:events.span")}</span>
        <button type="button" className={cn("rounded-md px-1.5 py-0.5 type-footnote tabular-nums", zoom.length ? "text-muted-foreground hover:bg-muted hover:text-foreground" : "font-medium text-foreground")} onClick={() => { setZoom([]); setSel(null) }}>{t("stats:events.whole")}</button>
        {zoom.map((w, i) => <span key={i} className="inline-flex items-center gap-2"><span className="text-tertiary">/</span>
          <button type="button" className={cn("anim-in rounded-md px-1.5 py-0.5 type-footnote tabular-nums", i === zoom.length - 1 ? "bg-muted font-medium text-foreground" : "text-muted-foreground hover:bg-muted hover:text-foreground")} onClick={() => { setZoom(zoom.slice(0, i + 1)); setSel(null) }} data-zoom-level={i}>{at(w[0], true)} – {at(w[1], true)}</button></span>)}
        <span className="flex-1" />
        {sel && <span className="anim-in inline-flex h-6 items-center gap-1 rounded-md bg-primary/10 pr-1 pl-2 type-footnote tabular-nums text-foreground" data-events-window>
          <span className="text-muted-foreground">{t("stats:events.time")}</span>{at(sel[0], long)} – {at(sel[1], long)}
          <button type="button" aria-label={t("stats:events.clear")} className="ml-0.5 rounded-md p-0.5 text-muted-foreground hover:text-foreground" onClick={() => setSel(null)}><XIcon className="size-3" /></button></span>}
      </div>
      <div className="border-t-0! px-5 pb-2">
        {!v ? <Loading h={150} err={d.s === "error" ? d.msg : null} />
          : <Brush v={v} sel={sel} long={long} onSel={setSel} onZoom={zoomIn} canOut={zoom.length > 0} onOut={() => { setZoom(zoom.slice(0, -1)); setSel(null) }} />}
      </div>
      {v && <StatStrip items={[
        { k: "turn", label: t("stats:events.turns"), value: fmtN(v.counts.turn) },
        { k: "decision", label: t("stats:events.decisions"), value: fmtN(v.counts.decision) },
        { k: "mark", label: t("stats:events.marks"), value: fmtN(v.counts.mark) },
        { k: "bad", label: t("stats:events.bad"), value: fmtN(v.counts.bad), tone: v.counts.bad ? "bad" : undefined },
      ]} />}
      {v && <EventList v={v} long={long} />}
      {v && <PanelFoot meta={<>{v.total > v.rows.length ? t("stats:events.shown", { count: v.total, shown: v.rows.length }) : t("stats:events.all_n", { count: v.total })}{v.capped && <> · {t("stats:events.capped")}</>}</>} />}
    </Panel>
  )
}

const H = 184, TOP = 44, AXIS = 20
/** the histogram with its sliding window */
function Brush({ v, sel, long, onSel, onZoom, canOut, onOut }: { v: Events; sel: Win | null; long: boolean; onSel: (w: Win | null) => void; onZoom: (w: Win) => void; canOut: boolean; onOut: () => void }) {
  const t = useT()
  const box = useRef<HTMLDivElement>(null)
  const [w, setW] = useState(800)
  useLayoutEffect(() => { const el = box.current; if (!el) return; const ro = new ResizeObserver(() => setW(el.clientWidth || 800)); ro.observe(el); setW(el.clientWidth || 800); return () => ro.disconnect() }, [])
  const h = v.histogram, n = h.turn.length
  const counts = useMemo(() => h.turn.map((x, i) => x + h.decision[i] + h.mark[i]), [h])
  const max = Math.max(1, ...counts)
  const span = v.hi - v.lo
  const x = (ts: number) => ((ts - v.lo) / span) * w
  const tsOf = (px: number) => v.lo + Math.min(1, Math.max(0, px / w)) * span
  const snap = (ts: number) => Math.min(v.hi, Math.max(v.lo, v.lo + Math.round((ts - v.lo) / h.step) * h.step))
  /* the window while dragging, then the one asked for */
  const [live, setLive] = useState<Win | null>(null)
  const [hover, setHover] = useState<number | null>(null)
  /* what a press here would do, shown as the pointer: resize at an edge of the window, slide inside it, else draw a new one */
  const [cursor, setCursor] = useState("cursor-crosshair")
  /* the floating labels over the window, measured so they stay inside the chart on a phone */
  const label = useRef<HTMLDivElement>(null)
  const [lw, setLw] = useState(0)
  useLayoutEffect(() => { setLw(label.current?.offsetWidth ?? 0) })
  const win = live ?? sel
  const drag = useRef<{ mode: "new" | "move" | "a" | "b"; x0: number; w0: Win } | null>(null)
  const inWin = (i: number) => !!win && h.start + (i + 1) * h.step > win[0] && h.start + i * h.step < win[1]
  const winCount = win ? counts.reduce((s, c, i) => s + (inWin(i) ? c : 0), 0) : 0
  const px = (e: RPointerEvent) => e.clientX - (box.current?.getBoundingClientRect().left ?? 0)
  const down = (e: RPointerEvent<SVGSVGElement>) => {
    const p = px(e)
    e.currentTarget.setPointerCapture(e.pointerId)
    if (win) {
      const [xa, xb] = [x(win[0]), x(win[1])]
      if (Math.abs(p - xa) < 7) { drag.current = { mode: "a", x0: p, w0: win }; return }
      if (Math.abs(p - xb) < 7) { drag.current = { mode: "b", x0: p, w0: win }; return }
      if (p > xa && p < xb) { drag.current = { mode: "move", x0: p, w0: win }; setCursor("cursor-grabbing"); return }
    }
    const ts = tsOf(p)
    drag.current = { mode: "new", x0: p, w0: [ts, ts] }
    setLive([ts, ts])
  }
  const move = (e: RPointerEvent<SVGSVGElement>) => {
    const p = px(e)
    const g = drag.current
    if (!g) {
      setHover(Math.min(n - 1, Math.max(0, Math.floor((p / w) * n))))
      const [xa, xb] = win ? [x(win[0]), x(win[1])] : [NaN, NaN]
      setCursor(Math.abs(p - xa) < 7 || Math.abs(p - xb) < 7 ? "cursor-ew-resize" : p > xa && p < xb ? "cursor-grab" : "cursor-crosshair")
      return
    }
    const dt = ((p - g.x0) / w) * span
    const [a, b] = g.w0
    if (g.mode === "new") { const t1 = tsOf(p); setLive(t1 < a ? [t1, a] : [a, t1]) }
    else if (g.mode === "move") { const len = b - a, na = Math.min(v.hi - len, Math.max(v.lo, a + dt)); setLive([na, na + len]) }
    else if (g.mode === "a") { const na = Math.min(b - h.step, Math.max(v.lo, a + dt)); setLive([na, b]) }
    else { const nb = Math.max(a + h.step, Math.min(v.hi, b + dt)); setLive([a, nb]) }
  }
  const up = () => {
    const g = drag.current
    drag.current = null
    if (!g) return
    if (g.mode === "move") setCursor("cursor-grab")
    const l = live ?? g.w0
    setLive(null)
    if (g.mode === "new" && Math.abs(x(l[1]) - x(l[0])) < 4) {
      /* a click: that bucket */
      const i = Math.min(n - 1, Math.max(0, Math.floor(((l[0] - v.lo) / span) * n)))
      onSel([h.start + i * h.step, Math.min(v.hi, h.start + (i + 1) * h.step)]); return
    }
    const a = snap(l[0]), b = snap(l[1])
    onSel(b > a ? [a, b] : [a, Math.min(v.hi, a + h.step)])
  }
  const key = (e: KeyboardEvent) => {
    if (e.key === "Escape") { onSel(null); return }
    if (!win) {
      if (e.key === "ArrowLeft" || e.key === "ArrowRight") { e.preventDefault(); const len = h.step * Math.max(1, Math.round(n / 8)); onSel([v.hi - len, v.hi]) }
      return
    }
    const [a, b] = win, s = h.step
    if (e.key === "Enter") { e.preventDefault(); onZoom(win); return }
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return
    e.preventDefault()
    const d = e.key === "ArrowRight" ? s : -s
    if (e.shiftKey) onSel([a, Math.max(a + s, Math.min(v.hi, b + d))])
    else if (a + d >= v.lo && b + d <= v.hi) onSel([a + d, b + d])
  }
  const slot = w / n, bw = Math.max(1, slot - (slot > 5 ? 2 : 1)), base = H - AXIS
  const scale = (c: number) => (c ? Math.max(2, (c / max) * (base - TOP)) : 0)
  const ticks = timeTicks(v.lo, v.hi, w)
  const xa = win ? x(win[0]) : 0, xb = win ? x(win[1]) : 0
  /** the left of a label of width `lw` centred on `c`, kept inside the chart */
  const place = (c: number) => Math.max(0, Math.min(c - lw / 2, w - lw))
  return (
    <div ref={box} className="relative w-full select-none" data-brush>
      {win && !live && (
        <div ref={label} className="anim-in absolute top-0 z-10 flex items-center gap-1 whitespace-nowrap rounded-lg bg-popover/95 py-0.5 pr-0.5 pl-2.5 text-popover-foreground shadow-md backdrop-blur"
          style={{ left: place((xa + xb) / 2) }} data-brush-label>
          <span className="type-footnote font-medium tabular-nums">{at(win[0], long)} – {at(win[1], long)}</span>
          <span className="type-footnote text-muted-foreground">· {t("stats:events.count", { count: v.a === win[0] && v.b === win[1] ? v.total : winCount })}</span>
          <Button size="icon-xs" variant="ghost" aria-label={t("stats:events.zoomIn")} title={t("stats:events.zoomIn")} onClick={() => onZoom(win)} data-zoom-in><ZoomInIcon /></Button>
          {canOut && <Button size="icon-xs" variant="ghost" aria-label={t("stats:events.zoomOut")} title={t("stats:events.zoomOut")} onClick={onOut} data-zoom-out><ZoomOutIcon /></Button>}
          <Button size="icon-xs" variant="ghost" aria-label={t("stats:events.clear")} title={t("stats:events.clear")} onClick={() => onSel(null)}><XIcon /></Button>
        </div>
      )}
      {live && <div ref={label} className="pointer-events-none absolute top-1 z-10 whitespace-nowrap rounded-md bg-foreground px-2 py-0.5 type-footnote tabular-nums text-background" style={{ left: place((xa + xb) / 2) }}>{at(live[0], long)} – {at(live[1], long)} · {t("stats:events.count", { count: winCount })}</div>}
      {!win && canOut && <Button size="xs" variant="ghost" className="absolute top-0 right-0 z-10 text-muted-foreground" onClick={onOut} data-zoom-out><ZoomOutIcon />{t("stats:events.zoomOut")}</Button>}
      <svg width={w} height={H} role="img" aria-label={t("stats:events.aria")} tabIndex={0} onKeyDown={key}
        className={cn("block touch-none overflow-visible rounded-md outline-none focus-visible:ring-2 focus-visible:ring-ring", cursor)}
        onPointerDown={down} onPointerMove={move} onPointerUp={up} onPointerCancel={() => { drag.current = null; setLive(null) }} onPointerLeave={() => setHover(null)}>
        {win && <rect x={xa} y={TOP - 6} width={Math.max(1, xb - xa)} height={base - TOP + 6} rx={6} fill="var(--primary)" opacity={0.07} />}
        {counts.map((c, i) => {
          const hh = scale(c), lit = win ? inWin(i) : hover === i
          return <rect key={i} x={i * slot + (slot - bw) / 2} y={base - hh} width={bw} height={hh} rx={Math.min(2, bw / 2)}
            fill={lit ? "var(--primary)" : "var(--muted-foreground)"} opacity={win ? (lit ? 0.9 : 0.22) : lit ? 0.9 : 0.4} className="transition-[opacity,fill] duration-[120ms]" data-bucket={i} />
        })}
        {h.bad.map((b, i) => b > 0 && (
          <g key={"bad" + i} data-bad={i}>
            <rect x={i * slot + (slot - bw) / 2} y={base - scale(b * (counts[i] ? 1 : 0))} width={bw} height={Math.min(scale(b), scale(counts[i]))} rx={Math.min(2, bw / 2)} fill="var(--destructive)" opacity={win && !inWin(i) ? 0.35 : 0.9} />
            <circle cx={i * slot + slot / 2} cy={base - scale(counts[i]) - 7} r={2.5} fill="var(--destructive)" opacity={win && !inWin(i) ? 0.4 : 1}><title>{t("stats:events.badN", { count: b })}</title></circle>
          </g>
        ))}
        {win && <>
          <line x1={xa} x2={xa} y1={TOP - 8} y2={base} stroke="var(--primary)" strokeWidth={1.5} strokeLinecap="round" className="cursor-ew-resize" />
          <line x1={xb} x2={xb} y1={TOP - 8} y2={base} stroke="var(--primary)" strokeWidth={1.5} strokeLinecap="round" className="cursor-ew-resize" />
        </>}
        <line x1={0} x2={w} y1={base} y2={base} stroke="var(--border)" />
        {ticks.map((ts) => { const tx = x(ts); return <g key={ts} data-tick={ts}>
          <line x1={tx} x2={tx} y1={base} y2={base + 4} stroke="var(--border)" />
          <text x={tx} y={H - 5} textAnchor={tx < 24 ? "start" : tx > w - 24 ? "end" : "middle"} className="fill-tertiary type-footnote tabular-nums">{tickLabel(ts)}</text>
        </g> })}
      </svg>
      {hover != null && !win && !drag.current && counts[hover] > 0 && (
        <div className="pointer-events-none absolute z-10 rounded-lg bg-popover px-2.5 py-1 type-footnote tabular-nums text-popover-foreground shadow-md" style={{ left: Math.min(Math.max(0, hover * slot - 50), w - 170), top: TOP - 4 }} data-brush-tip>
          {at(h.start + hover * h.step, long)} – {at(Math.min(v.hi, h.start + (hover + 1) * h.step), long)} · {t("stats:events.count", { count: counts[hover] })}{h.bad[hover] ? <span className="text-destructive"> · {t("stats:events.badN", { count: h.bad[hover] })}</span> : null}
        </div>
      )}
    </div>
  )
}

function EventList({ v, long }: { v: Events; long: boolean }) {
  const t = useT()
  /* the first rows, then more on request: one scrolling page, no scroll box inside it */
  const [all, setAll] = useState(false)
  if (!v.rows.length) return <div className="anim-in px-5 py-8 text-center type-callout text-muted-foreground">{t("stats:events.empty")}</div>
  const shown = all ? v.rows : v.rows.slice(0, 30)
  return (
    <div className="px-2 py-1.5" data-event-list>
      {shown.map((e) => {
        const open = () => go(e.kind === "turn" && e.session_id ? "#/sessions/" + e.session_id : e.item_id ? "#/items/" + e.item_id : "#/inbox")
        const detail = e.kind === "turn"
          ? [e.dur_ms != null ? fmtMs(e.dur_ms) : "", e.tokens ? fmtN(e.tokens) + " tokens" : "", e.tools ? t("stats:events.tools", { count: e.tools }) + (e.tool_errors ? " · " + t("stats:events.toolErrors", { count: e.tool_errors }) : "") : ""].filter(Boolean).join(" · ")
          : e.kind === "decision" ? (e.dur_ms != null ? t("stats:events.answered", { dur: fmtMs(e.dur_ms) }) : "") : e.by ?? ""
        return (
          <button key={e.kind + e.id} type="button" onClick={open} className="grid w-full grid-cols-[minmax(0,7.5rem)_minmax(0,1fr)] items-center gap-x-4 rounded-lg px-3 py-2 text-left hover:bg-muted/60 @3xl/events:grid-cols-[7.5rem_minmax(0,14rem)_minmax(0,1fr)_auto]" data-event={e.kind} data-event-bad={e.bad || undefined}>
            <span className="font-mono type-footnote tabular-nums text-muted-foreground">{at(e.ts, long, true)}</span>
            <span className="flex min-w-0 items-center gap-2 type-callout">
              <span aria-hidden className={cn("size-2 shrink-0 rounded-full", e.open && "animate-pulse motion-reduce:animate-none")} style={{ background: e.bad ? "var(--destructive)" : KIND_COLOR[e.kind] }} />
              <span className={cn("truncate", e.bad && "text-destructive")}>{eventLabel(e)}</span>
            </span>
            <span className="col-span-2 flex min-w-0 items-center gap-2 pl-[8.5rem] @3xl/events:col-span-1 @3xl/events:pl-0">
              {e.item_id ? <><ItemNo id={e.item_id} /><span className="truncate type-callout text-foreground/90">{e.title}</span></> : <span className="type-footnote text-tertiary">{t("stats:events.noItem")}</span>}
            </span>
            <span className="hidden max-w-80 truncate text-right type-footnote tabular-nums text-muted-foreground @3xl/events:block" title={detail || undefined}>{detail}</span>
          </button>
        )
      })}
      {v.rows.length > shown.length && <div className="px-1 pt-1 pb-1"><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setAll(true)} data-events-more>{t("stats:overview.showMore", { count: v.rows.length - shown.length })}</Button></div>}
    </div>
  )
}
