/* affectUI Pro – licensed under https://www.affectui.com/license, not under this project's open-source license. Don't copy it out of this project. */
import { useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from "react"

/*
 * Agent Lanes – who worked on what and when, a lane per agent, in a single self-contained file.
 * Adapted for Steve from affectUI's live lanes: the window is the one the work took (a span of hours for an earlier
 * round, reaching "now" only while something runs) instead of a sliding last half hour; a lane holds as many rows
 * as its agent ran things at once; blocks select instead of opening their own popover (Steve shows the detail
 * beside the page and decides cards on their items), and every word comes from the caller, so it follows i18n.
 *   · done blocks are quiet, a running one reaches "now" and shimmers, a failed one is red, a stopped one hatched
 *   · a block whose agent waits for a person is amber and carries a small flag after its title
 *   · a block is never narrower than a 24 px target: a run of seconds takes that much room, on a row of its own
 *     if it would cover another, and one at the window's end moves left to stay inside it
 *   · an earlier try of a step is dimmed on its lane; a hand-off draws a thin curve from one block to the next
 * The styles are injected below, so no extra files are needed.
 */

export type LaneAgent = {
  id: string
  name: string
  /** Model or role, shown under the name */
  model?: string
  /** A small mark – defaults to the initials on a soft tile */
  avatar?: ReactNode
  /** A short tag after the name, e.g. the coordinator's role */
  tag?: string
}

export type LaneTaskStatus = "done" | "running" | "starting" | "waiting" | "blocked" | "paused" | "failed" | "stopped"

export type LaneTask = {
  id: string
  agentId: string
  title: string
  status: LaneTaskStatus
  start: number
  end: number
  /** Ids of the blocks this one picked the work up from – drawn as hand-off curves */
  after?: string[]
  /** An earlier try of a step: dimmed */
  earlier?: boolean
  /** The task began before (or runs past) the window: that edge is cut, not rounded */
  cutStart?: boolean
  cutEnd?: boolean
  /** The native tooltip: what it was and what it took */
  tip?: string
  /** Read by assistive technology instead of the title */
  label?: string
  /** Extra attributes on the block (test and styling hooks) */
  attrs?: Record<string, string | number | undefined>
}

export type AgentLanesProps = {
  agents: LaneAgent[]
  tasks: LaneTask[]
  /** The window the lanes show, in ms since the epoch */
  range: [number, number]
  /** Drawn as a line when it falls inside the window – pass it only while something runs */
  now?: number
  /** A time on the axis, e.g. "15:30" */
  tick: (t: number) => string
  labels: { now: string; needsYou: string; lane: (agent: LaneAgent) => string }
  selected?: string | null
  onSelect?: (task: LaneTask) => void
  /** Under the lanes, inside the card */
  footer?: ReactNode
  className?: string
  style?: CSSProperties
}

const MIN = 60_000
/** Tick steps – the axis gets at most five labels, so they never run into each other */
const STEPS = [1, 2, 5, 10, 15, 30, 60, 120, 180, 360, 720, 1440, 2880, 10080].map((m) => m * MIN)
const SUB = 34
const PAD = 7
/** On narrow cards the names column is only avatars, so each lane names its agent above its blocks */
const HEAD = 14
/** The smallest block, in px: a target to tap, and room for a state's colour to read */
const MIN_PX = 24
const NARROW = 560
const laneHeight = (rows: number, head = 0) => head + PAD * 2 + Math.max(1, rows) * SUB

// The first letter or digit of the first two words – whole characters, so an emoji never splits in half
const initials = (name: string) =>
  name.split(/\s+/).map((w) => Array.from(w).find((c) => /[\p{L}\p{N}]/u.test(c))).filter(Boolean).join("").slice(0, 2).toUpperCase()

type Box = { start: number; end: number }
/** Where each block is drawn: at least `min` long (the smallest block in time), and inside the window */
function boxes(tasks: LaneTask[], from: number, to: number, min: number) {
  const out = new Map<string, Box>()
  for (const t of tasks) {
    const start = Math.max(from, Math.min(t.start, to - min))
    out.set(t.id, { start, end: Math.min(to, Math.max(t.end, start + min)) })
  }
  return out
}

/** Each agent's blocks spread over as few rows as keep them from overlapping as drawn (with their smallest width);
 * one that starts within `tol` of where the last ended (a hand-off) stays on its row */
function pack(tasks: LaneTask[], box: Map<string, Box>, tol: number) {
  const row = new Map<string, number>()
  const ends: number[] = []
  const at = (t: LaneTask) => box.get(t.id)!
  for (const t of [...tasks].sort((a, b) => at(a).start - at(b).start || at(a).end - at(b).end)) {
    const b = at(t)
    let r = ends.findIndex((e) => e - tol <= b.start)
    if (r < 0) { r = ends.length; ends.push(0) }
    ends[r] = b.end
    row.set(t.id, r)
  }
  return { row, rows: Math.max(1, ends.length) }
}

const HandIcon = () => (
  <svg width="10" height="10" viewBox="0 0 12 12" aria-hidden="true">
    <path d="M6 1.8 10.8 10H1.2L6 1.8Z" fill="currentColor" />
  </svg>
)

export function AgentLanes({ agents, tasks, range, now, tick, labels, selected, onSelect, footer, className, style }: AgentLanesProps) {
  const [from, to] = range
  const span = Math.max(to - from, MIN)
  const x = (t: number) => ((t - from) / span) * 100
  const shown = now !== undefined && now >= from && now <= to + 1000

  // The card's and the track's widths: how long the smallest block is in time, and whether the card is narrow
  const card = useRef<HTMLDivElement>(null)
  const track = useRef<HTMLDivElement>(null)
  const [size, setSize] = useState({ card: 0, track: 0 })
  useLayoutEffect(() => {
    const c = card.current, t = track.current
    if (!c || !t) return
    const fit = () => setSize((s) => s.card === c.clientWidth && s.track === t.clientWidth ? s : { card: c.clientWidth, track: t.clientWidth })
    fit()
    const ro = new ResizeObserver(fit)
    ro.observe(c); ro.observe(t)
    return () => ro.disconnect()
  }, [])
  const narrow = size.card > 0 && size.card <= NARROW
  const head = narrow ? HEAD : 0
  const px = span / Math.max(size.track || 600, 1)

  // Where each block is drawn, rows per agent, and where each lane starts
  const layout = useMemo(() => {
    const box = boxes(tasks, from, to, MIN_PX * px)
    const lanes = agents.map((a) => ({ agent: a, ...pack(tasks.filter((t) => t.agentId === a.id), box, 2 * px) }))
    let top = 0
    const tops = new Map<string, number>()
    for (const l of lanes) { tops.set(l.agent.id, top); top += laneHeight(l.rows, head) }
    return { box, lanes, tops, height: top }
  }, [agents, tasks, from, to, px, head])
  const boxOf = (t: LaneTask) => layout.box.get(t.id) ?? t
  const yOf = (t: LaneTask) => (layout.tops.get(t.agentId) ?? 0) + head + PAD + (layout.lanes.find((l) => l.agent.id === t.agentId)?.row.get(t.id) ?? 0) * SUB + SUB / 2

  // Round times across the window, labelled by the caller
  const ticks = useMemo(() => {
    const step = STEPS.find((s) => span / s <= 5) ?? STEPS[STEPS.length - 1]
    const out: number[] = []
    // whole hours and days fall on the local clock, not on UTC
    const offset = new Date(from).getTimezoneOffset() * MIN
    for (let t = Math.ceil((from - offset) / step) * step + offset; t <= to && out.length < 8; t += step) if (t > from) out.push(t)
    return out
  }, [from, to, span])

  // Hand-offs: from the end of one block to the start of the block that picked the work up
  const byId = new Map(tasks.map((t) => [t.id, t]))
  const curves = tasks.flatMap((t) => (t.after ?? []).flatMap((id) => {
    const prev = byId.get(id)
    if (!prev) return []
    const x1 = x(boxOf(prev).end) * 10, x2 = x(boxOf(t).start) * 10, y1 = yOf(prev), y2 = yOf(t)
    if (y1 === y2 && x2 >= x1) return []
    const bend = Math.max(24, Math.abs(x2 - x1) / 2)
    return [{ id: `${prev.id}>${t.id}`, d: `M ${x1} ${y1} C ${x1 + bend} ${y1}, ${x2 - bend} ${y2}, ${x2} ${y2}`, live: t.status === "running" || t.status === "starting" }]
  }))

  // On a narrow page the track scrolls sideways; it opens at "now" while something runs, else at the latest block
  // that went wrong or waits (or the latest of all), so the way the work ended is in view
  const scroller = useRef<HTMLDivElement>(null)
  const opened = useRef(false)
  useLayoutEffect(() => {
    const el = scroller.current
    if (!el || opened.current || !size.track || el.scrollWidth <= el.clientWidth) return
    opened.current = true
    if (shown) { el.scrollLeft = el.scrollWidth; return }
    const latest = (xs: LaneTask[]) => xs.reduce<LaneTask | null>((a, t) => !a || boxOf(t).end > boxOf(a).end ? t : a, null)
    const focus = latest(tasks.filter((t) => t.status === "failed" || t.status === "waiting" || t.status === "blocked")) ?? latest(tasks)
    if (!focus) return
    const b = boxOf(focus), w = size.track
    el.scrollLeft = Math.max(0, ((b.end - from) / span) * w - el.clientWidth + 24)
  })

  return (
    <div className={["ui-al", className].filter(Boolean).join(" ")} style={style}>
      <style href="ui-agent-lanes" precedence="default">{css}</style>
      <div className="ui-al__card" ref={card} data-narrow={narrow ? "" : undefined}>
        <div className="ui-al__body">
          {/* Who */}
          <ul className="ui-al__agents">
            {layout.lanes.map(({ agent: a, rows }) => {
              const waits = tasks.some((t) => t.agentId === a.id && t.status === "waiting")
              return (
                <li key={a.id} className="ui-al__agent" style={{ height: laneHeight(rows, head), paddingTop: head }} title={narrow ? a.name : undefined}>
                  <span className="ui-al__agent-row">
                    <span className="ui-al__avatar" data-logo={a.avatar ? "" : undefined}>{a.avatar ?? initials(a.name)}</span>
                    <span className="ui-al__who" data-state={waits ? "waiting" : undefined}>
                      <span className="ui-al__name-line">
                        <span className="ui-al__name" dir="auto" title={a.name}>{a.name}</span>
                        {waits ? <span className="ui-al__state"><span>{labels.needsYou}</span></span>
                          : a.tag && <span className="ui-al__state"><span>{a.tag}</span></span>}
                      </span>
                      {a.model && <span className="ui-al__model"><span className="ui-al__model-name" title={a.model}>{a.model}</span></span>}
                    </span>
                  </span>
                </li>
              )
            })}
          </ul>

          {/* When */}
          <div className="ui-al__scroll" ref={scroller}>
            <div className="ui-al__track" ref={track} style={{ height: layout.height }}>
              <div className="ui-al__grid" aria-hidden="true">
                {/* a tick right at "now" would run into its label */}
                {ticks.filter((t) => !shown || Math.abs(t - now!) > span * 0.08).map((t) => (
                  <span key={t} className="ui-al__tick" style={{ left: `${x(t)}%` }}><span>{tick(t)}</span></span>
                ))}
              </div>
              {layout.lanes.slice(1).map(({ agent: a }) => <span key={a.id} className="ui-al__sep" style={{ top: layout.tops.get(a.id) }} aria-hidden="true" />)}

              <svg className="ui-al__curves" viewBox={`0 0 1000 ${Math.max(1, layout.height)}`} preserveAspectRatio="none" aria-hidden="true">
                {curves.map((c) => <path key={c.id} d={c.d} data-live={c.live ? "" : undefined} data-handoff={c.id} vectorEffect="non-scaling-stroke" />)}
              </svg>

              {layout.lanes.map(({ agent: a, row, rows }) => (
                <ul key={a.id} className="ui-al__lane" style={{ top: layout.tops.get(a.id), height: laneHeight(rows, head) }} aria-label={labels.lane(a)}>
                  {narrow && <li className="ui-al__lane-name" aria-hidden="true"><span dir="auto">{a.name}</span></li>}
                  {tasks.filter((t) => t.agentId === a.id).map((t) => {
                    const b = boxOf(t), left = x(b.start), width = Math.max(0, x(b.end) - left)
                    return (
                      <li key={t.id} className="ui-al__slot" data-status={t.status} style={{ left: `${left}%`, width: `${width}%`, top: head + PAD + (row.get(t.id) ?? 0) * SUB + 4 }}>
                        <button type="button" className="ui-al__block" data-status={t.status} title={t.tip ?? t.title} aria-label={t.label ?? t.title}
                          aria-pressed={onSelect ? selected === t.id : undefined} data-selected={selected === t.id ? "" : undefined}
                          data-earlier={t.earlier ? "" : undefined} data-cut-start={t.cutStart ? "" : undefined} data-cut-end={t.cutEnd ? "" : undefined}
                          onClick={() => onSelect?.(t)} {...t.attrs}>
                          <span className="ui-al__label" dir="auto">{t.title}</span>
                          {t.status === "waiting" && <span className="ui-al__flag" aria-hidden="true"><HandIcon />{labels.needsYou}</span>}
                        </button>
                      </li>
                    )
                  })}
                </ul>
              ))}

              {shown && <span className="ui-al__now" style={{ left: `${x(now!)}%` }} aria-hidden="true"><span>{labels.now}</span></span>}
            </div>
          </div>
        </div>
        {footer}
      </div>
    </div>
  )
}

const css = /* css */ `
/* Colors and motion as variables – override them on .ui-al */
.ui-al {
  --ui-al-strong: var(--aui-ink, #171717);
  --ui-al-ink: var(--aui-ink, #171717);
  --ui-al-text: var(--aui-muted, #6b6b6b);
  --ui-al-muted: var(--aui-muted, #8a8a8a);
  --ui-al-faint: var(--aui-placeholder, #b8b8b8);
  --ui-al-line: color-mix(in srgb, var(--aui-border, #ececec) 70%, transparent);
  --ui-al-rail: var(--aui-placeholder, #c4c4c8);
  --ui-al-fill: var(--aui-subtle, #ececec);
  --ui-al-done: color-mix(in srgb, var(--aui-ink, #171717) 7%, transparent);
  --ui-al-done-hover: color-mix(in srgb, var(--aui-ink, #171717) 11%, transparent);
  --ui-al-run-bg: color-mix(in srgb, var(--aui-blue, #0190fe) 14%, transparent);
  --ui-al-run-fg: var(--aui-blue, #4fb0fe);
  --ui-al-run-edge: color-mix(in srgb, var(--aui-blue, #0190fe) 60%, transparent);
  --ui-al-warn-bg: var(--aui-warning-soft, #fff1e0);
  --ui-al-warn-fg: var(--aui-warning, #b25e00);
  --ui-al-danger-bg: var(--aui-danger-soft, #fdecea);
  --ui-al-danger-fg: var(--aui-danger, #c4320a);
  --ui-al-focus: var(--aui-focus, rgba(0, 96, 226, 0.7));
  --ui-al-shadow: var(--aui-shadow-card, 0 0 0 1px #ececec, 0 1px 2px rgba(0, 0, 0, 0.04));
  --ui-al-agents: 176px;
  --ui-al-min: 0px;
  position: relative;
  box-sizing: border-box;
  width: 100%;
  container-type: inline-size;
  font-family: var(--aui-font, ui-sans-serif, system-ui, sans-serif);
  font-size: 13px;
  line-height: 20px;
  color: var(--ui-al-text);
  text-align: left;
  -webkit-font-smoothing: antialiased;
}
/* The resets reach only the lanes' own parts: these styles are not in a cascade layer, so they would win over the
   utility classes of whatever the caller puts in the footer */
.ui-al__body *, .ui-al__body *::before, .ui-al__body *::after { box-sizing: border-box; }
:where(.ui-al__block) { margin: 0; padding: 0; border: 0; background: none; font: inherit; color: inherit; text-align: inherit; cursor: pointer; -webkit-tap-highlight-color: transparent; }
.ui-al__block:focus-visible { outline: 2px solid var(--ui-al-focus); outline-offset: 2px; }
:is(.ui-al__agents, .ui-al__lane) { margin: 0; padding: 0; list-style: none; }
.ui-al__body svg { display: block; flex: none; }

/* One card holds the lanes */
.ui-al__card { border-radius: var(--aui-radius-lg, 12px); background: var(--aui-surface, #fff); box-shadow: var(--ui-al-shadow); }

/* ----- Lanes ----- */
.ui-al__body { display: grid; grid-template-columns: var(--ui-al-agents) minmax(0, 1fr); padding: 30px 16px 10px 0; }
.ui-al__agent { min-width: 0; padding: 0 12px 0 16px; }
.ui-al__agent-row { display: flex; align-items: center; gap: 10px; height: ${PAD * 2 + SUB}px; min-width: 0; }
.ui-al__avatar { display: grid; flex: none; place-items: center; width: 24px; height: 24px; overflow: hidden; border-radius: 50%; background: var(--ui-al-fill); font-size: 10px; font-weight: 500; color: var(--ui-al-text); }
.ui-al__avatar[data-logo] { background: none; }
.ui-al__who { display: grid; min-width: 0; }
.ui-al__name { overflow: hidden; font-weight: 500; color: var(--ui-al-ink); white-space: nowrap; text-overflow: ellipsis; }
.ui-al__name-line { display: flex; align-items: center; min-width: 0; }
.ui-al__model { display: flex; min-width: 0; font-size: 12px; line-height: 16px; color: var(--ui-al-muted); white-space: nowrap; }
.ui-al__model-name { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
.ui-al__state { display: inline-flex; flex: none; margin-left: 6px; }
.ui-al__state > span { padding: 0 5px; border-radius: calc(var(--aui-radius-sm, 6px) - 2px); background: var(--ui-al-fill); font-size: 11px; font-weight: 500; line-height: 16px; color: var(--ui-al-text); white-space: nowrap; }
.ui-al__who[data-state="waiting"] .ui-al__state > span { background: var(--ui-al-warn-bg); color: var(--ui-al-warn-fg); }

.ui-al__scroll { min-width: 0; overflow-x: auto; overflow-y: visible; margin-top: -30px; padding-top: 30px; scrollbar-width: thin; }
.ui-al__track { position: relative; min-width: var(--ui-al-min); }
/* Quiet lane separators and time ticks */
.ui-al__sep { position: absolute; left: 0; right: 0; height: 0; border-top: 1px solid var(--ui-al-line); pointer-events: none; }
.ui-al__grid { position: absolute; inset: 0; pointer-events: none; }
.ui-al__tick { position: absolute; top: 0; bottom: 0; width: 0; border-left: 1px dashed var(--ui-al-line); }
.ui-al__tick > span { position: absolute; bottom: 100%; left: 0; margin-bottom: 8px; font-size: 11px; line-height: 14px; color: var(--ui-al-faint); white-space: nowrap; transform: translateX(-50%); font-variant-numeric: tabular-nums; }

.ui-al__curves { position: absolute; inset: 0; width: 100%; height: 100%; overflow: visible; pointer-events: none; }
.ui-al__curves path { fill: none; stroke: var(--ui-al-rail); stroke-width: 1; stroke-dasharray: 3 3; }
.ui-al__curves path[data-live] { stroke: var(--ui-al-run-edge); animation: ui-al-flow 900ms linear infinite; }

.ui-al__lane { position: absolute; left: 0; right: 0; }
.ui-al__slot { position: absolute; height: ${SUB - 8}px; min-width: ${MIN_PX}px; padding-right: 2px; }
.ui-al__block { position: relative; display: flex; align-items: center; gap: 6px; width: 100%; height: 100%; min-width: ${MIN_PX - 2}px; padding: 0 8px; overflow: hidden; border-radius: var(--aui-radius-sm, 6px); background: var(--ui-al-done); font-size: 12px; font-weight: 500; line-height: 16px; color: var(--ui-al-text); white-space: nowrap; transition: background-color 120ms ease, color 120ms ease, box-shadow 120ms ease; }
.ui-al__block { container-type: inline-size; }
/* A block too short for a word shows none, never a clipped letter */
@container (max-width: 34px) { .ui-al__label { display: none; } }
/* the flag needs the room of its word next to a word of the title; a shorter block keeps only its amber */
@container (max-width: 96px) { .ui-al__flag { display: none; } }
.ui-al__label { min-width: 0; overflow: hidden; text-overflow: ellipsis; text-align: start; }
.ui-al__block:hover { background: var(--ui-al-done-hover); color: var(--ui-al-ink); }
.ui-al__block:is([data-status="running"], [data-status="starting"]) { background: var(--ui-al-run-bg); color: var(--ui-al-run-fg); box-shadow: inset -2px 0 0 var(--ui-al-run-edge); }
.ui-al__block[data-status="starting"] { opacity: 0.75; }
.ui-al__block[data-status="running"]::after { content: ""; position: absolute; inset: 0; background: linear-gradient(90deg, transparent 0%, color-mix(in srgb, var(--aui-surface, #fff) 55%, transparent) 50%, transparent 100%) 0 0 / 50% 100% no-repeat; animation: ui-al-shimmer 2200ms ease-in-out infinite; }
.ui-al__block:is([data-status="waiting"], [data-status="blocked"]) { background: var(--ui-al-warn-bg); color: var(--ui-al-warn-fg); }
.ui-al__block[data-status="failed"] { background: var(--ui-al-danger-bg); box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--ui-al-danger-fg) 40%, transparent); color: var(--ui-al-danger-fg); }
/* Stopped: hatched, never filled – it does not read as done */
.ui-al__block[data-status="stopped"] { background: repeating-linear-gradient(135deg, var(--ui-al-done) 0 4px, transparent 4px 7px); box-shadow: inset 0 0 0 1px var(--ui-al-done-hover); color: var(--ui-al-muted); }
.ui-al__block[data-status="paused"] { background: var(--ui-al-fill); box-shadow: inset 0 0 0 1px var(--ui-al-done-hover); color: var(--ui-al-muted); }
.ui-al__block[data-earlier] { opacity: 0.6; }
/* The coordinator's own span is the backdrop of the work: quiet, whatever state it is in, unless it runs now */
.ui-al__block[data-coord]:not([data-status="running"]) { background: var(--ui-al-done); box-shadow: none; color: var(--ui-al-text); }
.ui-al__block[data-coord]::after { display: none; }
.ui-al__block[data-earlier] .ui-al__label { text-decoration: line-through; text-decoration-color: color-mix(in srgb, currentColor 45%, transparent); }
/* Cut by the window: the cut edge is square and fades */
.ui-al__block[data-cut-start] { border-top-left-radius: 0; border-bottom-left-radius: 0; -webkit-mask-image: linear-gradient(to right, transparent 0, #000 24px); mask-image: linear-gradient(to right, transparent 0, #000 24px); padding-left: 20px; }
.ui-al__block[data-cut-end] { border-top-right-radius: 0; border-bottom-right-radius: 0; -webkit-mask-image: linear-gradient(to left, transparent 0, #000 24px); mask-image: linear-gradient(to left, transparent 0, #000 24px); }
.ui-al__block[data-cut-start][data-cut-end] { -webkit-mask-image: linear-gradient(to right, transparent 0, #000 24px, #000 calc(100% - 24px), transparent); mask-image: linear-gradient(to right, transparent 0, #000 24px, #000 calc(100% - 24px), transparent); }
.ui-al__block[data-selected] { box-shadow: inset 0 0 0 1.5px var(--aui-primary, #171717); color: var(--ui-al-strong); opacity: 1; }

/* Needs you: a small flag at the block's end, after its title (which gives way to it) */
.ui-al__flag { display: inline-flex; flex: none; align-items: center; gap: 3px; margin-left: auto; margin-right: -4px; padding: 0 5px; border-radius: 4px; background: var(--aui-surface, #fff); box-shadow: var(--ui-al-shadow); font-size: 11px; font-weight: 500; line-height: 16px; color: var(--ui-al-warn-fg); white-space: nowrap; pointer-events: none; animation: ui-al-pulse 1600ms ease-in-out infinite; }
.ui-al__slot:has(.ui-al__block[data-status="waiting"]) { z-index: 2; }
/* Narrow cards: the agent's name over its lane, staying in view while the track scrolls */
.ui-al__lane-name { position: sticky; left: 0; display: block; width: max-content; max-width: 100%; height: ${HEAD}px; padding: 3px 0 0 2px; font-size: 11px; font-weight: 500; line-height: 13px; color: var(--ui-al-muted); white-space: nowrap; pointer-events: none; }
.ui-al__lane-name > span { display: block; max-width: 220px; overflow: hidden; text-overflow: ellipsis; }

/* Now: a thin line with a small label over it */
.ui-al__now { position: absolute; top: -6px; bottom: 0; width: 0; border-left: 1px solid var(--ui-al-run-edge); pointer-events: none; z-index: 3; }
.ui-al__now > span { position: absolute; bottom: 100%; right: -1px; padding: 0 5px; border-radius: 4px; background: var(--ui-al-run-bg); font-size: 10px; font-weight: 500; line-height: 14px; color: var(--ui-al-run-fg); white-space: nowrap; }

@keyframes ui-al-pulse { 50% { opacity: 0.55; } }
@keyframes ui-al-shimmer { from { background-position: -100% 0; } to { background-position: 200% 0; } }
@keyframes ui-al-flow { to { stroke-dashoffset: -6; } }

/* Narrow containers: the names column shrinks to the avatars, the track scrolls sideways */
@container (max-width: 560px) {
  /* a container query styles what is inside the container, so the widths are set on the card */
  .ui-al__card { --ui-al-agents: 44px; --ui-al-min: 520px; }
  .ui-al__agent { padding: 0 0 0 12px; }
  .ui-al__who { display: none; }
  .ui-al__body { padding-right: 0; }
  .ui-al__scroll { padding-right: 12px; }
}
@media (prefers-reduced-motion: reduce) {
  .ui-al *, .ui-al *::before, .ui-al *::after { animation: none !important; transition: none !important; }
}
`
