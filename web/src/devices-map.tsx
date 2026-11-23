/* The connection map on the devices overview: who talks to whom and how. Three columns on a desktop: the clients signed
 * in to the Hub (they reach only the Hub), the Hub and its own Node in one dashed frame (they are one computer), and the
 * remote Nodes the Hub hands sessions to. Two kinds of edges, told apart by line style as well as color: Hub → Node
 * (runs work) solid, in the Node's one status (device-model's nodeStatus), thicker with its sessions and moving slowly
 * while they run; client → Hub (access) thin blue dashes. A Node's route (this machine, LAN, tailnet, tailcat, HTTPS)
 * is labelled at its end. On a phone the map turns into a tree: clients above, the frame, the Nodes on one trunk.
 *
 * The cards are HTML (buttons, so Tab, arrow keys and Enter work; a hover card tells the facts behind one) and the
 * edges an SVG over them, computed from the cards' real boxes whenever any of them moves (ResizeObserver): text size,
 * width and counts never misplace an edge. The order is fixed while the page is open (device-map-model's keepOrder),
 * so cards do not jump; layout, order and shapes come only from the data. */
import { useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type ReactNode } from "react"
import { ChevronRightIcon, GlobeIcon, LaptopIcon, MonitorIcon, MonitorSmartphoneIcon, NetworkIcon, PlusIcon, ServerIcon, SmartphoneIcon, SquareTerminalIcon, UsersIcon } from "lucide-react"
import { useT } from "@/i18n"
import { num } from "@/i18n/format"
import { HoverCard } from "@/components/affect/hover-card-clean"
import { Checkbox } from "@/components/ui/checkbox"
import { go, openSheet } from "@/ui"
import { deviceWorkEvidence, st, useStore } from "@/store"
import type { AccessClient } from "@/access-api"
import { agoText, clientKind, hostLine, hostOf, linkRoute, loadPct, memPct, nodeStatus, offlineReason, one, routeLabel, rttOf, statusText, CLIENT_STALE_MS, LOCAL_NODE, type ClientKind, type NodeView } from "@/device-model"
import { clientOnline, edgeFlows, SELF_CLIENT, edgeOf, edgeWidth, keepOrder, mapClients, mapNodes, narrowEdges, wideEdges, MAP_COMPACT_FROM, MAP_MAX_NODES, type Box, type EdgeKind, type MapLabel } from "@/device-map-model"
import { useDevices } from "@/devices-data"
import { deviceGroups } from "@/devices-work"
import { hostIcon, nodeHref } from "@/devices-nodes"
import { Chip, DCard, StatusBadge, StatusDot } from "@/devices-ui"
import { cn } from "cn"

/* the wide map's columns and their smallest widths, in px: the CSS draws the grid from these (as --dmap-* variables), and
 * the map turns into a vertical tree whenever its own width (not the window's) is less than they add up to, so a column
 * is never cut off between a phone and a wide window */
const MAP_PAD_X = 24
const MAP_GAP_MIN = 68
const MAP_COL_MIN = { clients: 150, hub: 250, nodes: 230, hubNoClients: 230 }
/** the smallest width of the map, padding included, at which the wide (three or two column) layout fits */
const wideMinWidth = (clients: boolean) =>
  2 * MAP_PAD_X + (clients ? MAP_COL_MIN.clients + MAP_COL_MIN.hub + MAP_COL_MIN.nodes + 2 * MAP_GAP_MIN : MAP_COL_MIN.hubNoClients + MAP_COL_MIN.nodes + MAP_GAP_MIN)
const MAP_VARS = {
  "--dmap-pad-x": `${MAP_PAD_X}px`, "--dmap-gap-min": `${MAP_GAP_MIN}px`, "--dmap-col-c": `${MAP_COL_MIN.clients}px`,
  "--dmap-col-h": `${MAP_COL_MIN.hub}px`, "--dmap-col-n": `${MAP_COL_MIN.nodes}px`, "--dmap-col-h0": `${MAP_COL_MIN.hubNoClients}px`,
} as CSSProperties
/** where this client remembers whether the map shows the clients */
const SHOW_CLIENTS = "steve.devices.mapClients"
const GIB = 2 ** 30

const CLIENT_ICON: Record<ClientKind, typeof GlobeIcon> = { browser: GlobeIcon, phone: SmartphoneIcon, mac: MonitorIcon, cli: SquareTerminalIcon, other: MonitorSmartphoneIcon }

/** the map's state while it is on the screen: which card is pointed at or focused, and the narrow layout */
type Drawn = {
  edges: { id: string; d: string; kind: "exec" | "access"; width?: number; tone?: EdgeKind; flow?: boolean; me?: boolean; stale?: boolean }[]
  labels: (MapLabel & { text: string; tone?: EdgeKind; unknown?: boolean })[]
  trunk: string | null
}

/** keeps the order of `ids` as it was first seen while this component lives (see keepOrder) */
function useStableOrder(ids: string[]): string[] {
  const kept = useRef<string[] | null>(null)
  kept.current = kept.current ? keepOrder(kept.current, ids) : ids
  return kept.current
}

export function DeviceMap() {
  const t = useT(), { views, hubVersion, access, nodesOff, upgradeDefault, vitals } = useDevices()
  useStore()
  const now = Date.now()
  const [show, setShow] = useState(() => { try { return localStorage.getItem(SHOW_CLIENTS) !== "0" } catch { return true } })
  const toggle = (on: boolean) => { setShow(on); try { localStorage.setItem(SHOW_CLIENTS, on ? "1" : "0") } catch { /* private mode: for this visit only */ } }
  const canvas = useRef<HTMLDivElement>(null)
  const [narrow, setNarrow] = useState(false)
  const [hot, setHot] = useState<string | null>(null)
  const [drawn, setDrawn] = useState<Drawn>({ edges: [], labels: [], trunk: null })

  const local = views.find((v) => v.local)
  const sorted = mapNodes(views, hubVersion)
  const order = useStableOrder(sorted.map((v) => v.id))
  const remote = order.map((id) => sorted.find((v) => v.id === id)!).filter(Boolean)
  const shownNodes = remote.slice(0, MAP_MAX_NODES), hiddenNodes = remote.length - shownNodes.length
  const compact = remote.length >= MAP_COMPACT_FROM
  const clients = mapClients(access, t("devices:clients.browserSession"))
  const current = access && access.clients.some((c) => c.id === access.current && !c.revoked) ? access.current : SELF_CLIENT
  const clientOrder = useStableOrder(clients.shown.map((c) => c.id))
  const shownClients = show ? clientOrder.map((id) => clients.shown.find((c) => c.id === id)!).filter(Boolean) : []

  const live = views.filter((v) => v.state !== "revoked" && v.state !== "pending")
  const online = live.filter((v) => v.linked).length
  const running = live.reduce((n, v) => n + (v.running ?? 0), 0)
  const lv = vitals?.[LOCAL_NODE]?.latest
  const machine = local ? hostOf(local, lv) : null
  const machineName = local?.device?.name ?? local?.name ?? ""

  /* the edges, from the cards where they are now: after every render and whenever a card or the map changes size; the
   * state changes only when a shape did, so drawing them does not render again and again */
  const lastSig = useRef("")
  const layout = () => {
    const el = canvas.current
    if (!el) return
    /* the padding is the wide one in both layouts here, so the choice depends on the map's width alone and cannot flip back and forth */
    const isNarrow = el.clientWidth < wideMinWidth(show)
    setNarrow(isNarrow)
    const cr = el.getBoundingClientRect()
    const box = (x: Element | null): Box | null => { if (!x) return null; const r = x.getBoundingClientRect(); return { l: r.left - cr.left, t: r.top - cr.top, r: r.right - cr.left, b: r.bottom - cr.top } }
    const hub = box(el.querySelector('[data-map-card="hub"]')), frame = box(el.querySelector("[data-map-machine]"))
    if (!hub || !frame) return
    const localBox = box(el.querySelector(`[data-map-card="${LOCAL_NODE}"]`))
    const nodeBoxes = [...el.querySelectorAll<HTMLElement>("[data-map-node]")].map((x) => ({ id: x.dataset.mapNode!, box: box(x)! }))
    const clientBoxes = [...el.querySelectorAll<HTMLElement>("[data-map-client]")].map((x) => ({ id: x.dataset.mapClient!, box: box(x)! }))
    const g = isNarrow ? narrowEdges(hub, frame, localBox, nodeBoxes, clientBoxes) : { ...wideEdges(hub, localBox, nodeBoxes, clientBoxes), trunk: null }
    const byId = (id: string) => (id === LOCAL_NODE ? local : views.find((v) => v.id === id))
    const out: Drawn = { edges: [], labels: [], trunk: g.trunk }
    for (const e of g.edges) {
      if (e.id === "add") { out.edges.push({ ...e, kind: "exec", tone: "off", width: 1.5 }); continue }
      const v = byId(e.id)
      if (!v) continue
      const tone = edgeOf(nodeStatus(v, hubVersion))
      out.edges.push({ ...e, kind: "exec", tone, width: edgeWidth(v), flow: edgeFlows(v, tone) })
    }
    for (const e of g.access) {
      const c = clients.shown.find((x) => x.id === e.id)
      out.edges.push({ ...e, id: "c:" + e.id, kind: "access", me: e.id === current, stale: !!c && !clientOnline(c) && now - (c.last_seen ?? c.created) > CLIENT_STALE_MS })
    }
    for (const l of g.labels) {
      const v = byId(l.id)
      if (!v) continue
      const route = linkRoute(v)
      out.labels.push({ ...l, text: route ? t(`devices:route.${route}`) : "—", unknown: !route, tone: edgeOf(nodeStatus(v, hubVersion)) })
    }
    const sig = JSON.stringify(out)
    if (sig !== lastSig.current) { lastSig.current = sig; setDrawn(out) }
  }
  const layoutRef = useRef(layout)
  layoutRef.current = layout
  useLayoutEffect(() => { layout() })
  useLayoutEffect(() => {
    const el = canvas.current
    if (!el) return
    let frame = 0
    const again = () => { cancelAnimationFrame(frame); frame = requestAnimationFrame(() => layoutRef.current()) }
    const ro = new ResizeObserver(again)
    ro.observe(el)
    el.querySelectorAll("[data-map-card], [data-map-machine]").forEach((x) => ro.observe(x))
    void document.fonts?.ready.then(again)
    return () => { ro.disconnect(); cancelAnimationFrame(frame) }
  }, [shownNodes.length, shownClients.length, show])

  /* the arrow keys move within a column, as Tab moves through the whole map */
  const arrows = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return
    const cards = [...e.currentTarget.querySelectorAll<HTMLElement>("[data-map-card]")]
    const i = cards.indexOf(document.activeElement as HTMLElement)
    if (i < 0) return
    e.preventDefault()
    cards[Math.max(0, Math.min(cards.length - 1, i + (e.key === "ArrowDown" ? 1 : -1)))]?.focus()
  }
  /* pointing at or focusing a card lights its own edge and dims the others (not for the Hub, whose edges are all) */
  const point = (id: string) => ({
    onPointerEnter: (e: { pointerType: string }) => { if (e.pointerType !== "touch" && !narrow) setHot(id) },
    onPointerLeave: () => setHot((h) => (h === id ? null : h)),
    onFocus: () => { if (!narrow) setHot(id) },
    onBlur: () => setHot((h) => (h === id ? null : h)),
  })
  const dim = (id: string) => !!hot && hot !== "hub" && hot !== id

  const card = (id: string, cls: string, open: () => void, body: ReactNode, hover: ReactNode, data: Record<string, string>) => (
    <HoverCard key={id} content={hover} disabled={narrow} openDelay={350} side={id.startsWith("c:") ? "right" : id === "hub" || id === LOCAL_NODE ? "bottom" : "left"} align="start" width={288} className="steve-map-hover">
      <button type="button" className={cn("dmap-card", cls)} onClick={open} data-map-card={id} {...data} {...point(id)}>{body}</button>
    </HoverCard>
  )

  const hubFacts = <div className="dmap-hub-facts">
    <div><span>{t("devices:map.hubSessions")}</span><b>{t("devices:map.hubRunning", { n: running })}</b></div>
    <div><span>{t("devices:tab.nodes")}</span><b>{t("devices:map.hubOnline", { on: online, all: live.length })}</b></div>
    <div><span>{t("devices:tab.clients")}</span><b>{t("devices:map.clientsOnline", { n: clients.online })}</b></div>
  </div>

  return (
    <DCard data-device-map>
      <header className="flex flex-wrap items-start gap-x-4 gap-y-2 px-5 pt-4 pb-3">
        <div className="min-w-0 flex-1 basis-60">
          <h2 className="type-headline text-foreground">{t("devices:map.title")}</h2>
          <p className="type-footnote text-pretty text-muted-foreground">{t("devices:map.sub")}</p>
        </div>
        <div className="flex flex-wrap items-center gap-x-3.5 gap-y-1 type-footnote text-muted-foreground">
          <span className="inline-flex items-center gap-1.5" aria-hidden><svg viewBox="0 0 28 8" className="h-2 w-7"><path d="M1 4h26" stroke="var(--dmap-exec)" strokeWidth="2.2" fill="none" /></svg>{t("devices:map.exec")}</span>
          {show && <span className="inline-flex items-center gap-1.5" aria-hidden><svg viewBox="0 0 28 8" className="h-2 w-7"><path d="M1 4h26" stroke="var(--dmap-access)" strokeWidth="1.4" strokeDasharray="4 4" fill="none" /></svg>{t("devices:map.access")}</span>}
          <label className="inline-flex cursor-pointer items-center gap-1.5 text-foreground/80"><Checkbox checked={show} onCheckedChange={(on) => toggle(on === true)} data-map-show-clients />{t("devices:map.showClients")}</label>
        </div>
      </header>
      <div className="px-5 pb-5">
        <div ref={canvas} className="dmap" style={MAP_VARS} data-narrow={narrow || undefined} data-clients={show || undefined} data-hot={hot ?? undefined} data-map-compact={compact || undefined}>
          <svg className="dmap-edges" aria-hidden>
            {drawn.trunk && <path className="dmap-edge is-exec is-trunk" d={drawn.trunk} />}
            {drawn.edges.map((e) => <path key={e.kind + e.id} d={e.d} data-edge={e.id} data-edge-kind={e.kind === "access" ? "access" : e.tone}
              className={cn("dmap-edge", e.kind === "access" ? "is-access" : "is-exec", e.tone && `is-${e.tone}`, e.flow && "is-flow", e.me && "is-me", e.stale && "is-stale", dim(e.id) && "is-dim")}
              style={e.width ? { strokeWidth: e.width } : undefined} />)}
          </svg>
          {drawn.labels.map((l) => <span key={l.id} className={cn("dmap-label", l.tone && `is-${l.tone}`, l.unknown && "is-unknown", dim(l.id) && "is-dim")} style={{ left: l.x, top: l.y }}
            title={l.unknown ? t("devices:map.routeUnknown") : undefined} data-edge-label={l.id}>{l.text}</span>)}
          <div className="dmap-cols">
            {show && <div className="dmap-col dmap-clients" onKeyDown={arrows} data-map-clients>
              <div className="dmap-col-label"><UsersIcon aria-hidden />{t("devices:map.clients", { n: clients.total })}</div>
              {shownClients.map((c) => {
                const on = clientOnline(c), me = c.id === current, k = clientKind(c.kind, c.platform), I = CLIENT_ICON[k]
                const stale = !on && now - (c.last_seen ?? c.created) > CLIENT_STALE_MS
                return card("c:" + c.id, cn("is-client", stale && "is-stale"), () => go(me && c.id === SELF_CLIENT ? "#/devices/clients" : "#/devices/clients/" + encodeURIComponent(c.id)),
                  <><span className="dmap-l1"><span className={cn("dmap-dot", on ? "is-ok" : "is-off")} aria-hidden /><I className="dmap-ico" aria-hidden /><span className="dmap-name">{c.name}</span>{me && <Chip tone="info">{t("devices:clients.you")}</Chip>}</span>
                    <span className="dmap-l2"><span className="dmap-kind">{t(`devices:clients.kind.${k}`)}</span>{c.last_route && <span>{routeLabel(c.last_route)}</span>}<span>{on ? t("devices:status.online") : c.last_seen ? agoText(c.last_seen, now) : t("devices:tokens.neverUsed")}</span></span></>,
                  <ClientHover c={c} me={me} on={on} />, { "data-map-client": c.id })
              })}
              {clients.total > shownClients.length && <a className="dmap-more" href="#/devices/clients" data-map-more-clients>{t("devices:map.moreClients", { count: clients.total - shownClients.length })}<ChevronRightIcon aria-hidden /></a>}
            </div>}
            <div className="dmap-col dmap-center" onKeyDown={arrows}>
              <div className="dmap-machine" data-map-machine>
                <span className="dmap-machine-label" title={t("devices:map.thisComputer")}><LaptopIcon aria-hidden />
                  <span className="truncate">{[machineName, machine?.cpus ? t("devices:hub.cores", { n: machine.cpus }) : "", machine?.memGb ? t("devices:vitals.gb", { n: machine.memGb }) : ""].filter(Boolean).join(" · ") || t("devices:map.thisComputer")}</span></span>
                {card("hub", "is-hub", () => go("#/devices/hub"),
                  <><span className="dmap-l1"><span className="dmap-hub-mark" aria-hidden><NetworkIcon /></span><span className="dmap-name">Hub</span><StatusBadge status={{ kind: "online", tone: "ok" }} /></span>
                    <span className="dmap-l2"><span>{[t("devices:map.hubRole"), hubVersion].filter(Boolean).join(" · ")}{st.update && <> · <span className="text-blue">{t("devices:hub.updateAvailable", { version: st.update.version })}</span></>}</span></span>
                    {hubFacts}</>,
                  <HubHover nodesOff={nodesOff} policy={upgradeDefault?.policy} />, {})}
                {local && <LocalCard v={local} card={card} />}
              </div>
            </div>
            <div className="dmap-col dmap-nodes" onKeyDown={arrows} data-map-nodes>
              <div className="dmap-col-label"><ServerIcon aria-hidden />{t("devices:map.remote", { n: remote.length })}</div>
              {shownNodes.map((v) => <NodeMapCard key={v.id} v={v} compact={compact} narrow={narrow} card={card} />)}
              {hiddenNodes > 0 && <a className="dmap-more" href="#/devices/nodes" data-map-more-nodes>{t("devices:map.moreNodes", { count: hiddenNodes })}<ChevronRightIcon aria-hidden /></a>}
              {remote.length === 0 && <button type="button" className="dmap-card is-ghost" onClick={() => openSheet("addDevice", { kind: "node" })} data-map-card="add" data-map-node="add">
                <span className="dmap-l1"><PlusIcon className="dmap-ico text-foreground" aria-hidden /><span className="dmap-name">{t("devices:map.addNode")}</span></span>
                <span className="dmap-ghost-sub">{t("devices:map.addNodeSub", { hub: machineName || "Hub" })}</span>
              </button>}
            </div>
          </div>
        </div>
      </div>
    </DCard>
  )
}

type CardFn = (id: string, cls: string, open: () => void, body: ReactNode, hover: ReactNode, data: Record<string, string>) => ReactNode

/** the Hub's own Node, under the Hub in the frame */
function LocalCard({ v, card }: { v: NodeView; card: CardFn }) {
  const t = useT(), { hubVersion, vitals } = useDevices()
  const s = nodeStatus(v, hubVersion), l = v.linked ? vitals?.[v.id]?.latest : undefined, lp = loadPct(l), mp = memPct(l)
  const ask = v.device && deviceWorkEvidence().loaded ? deviceGroups(v.device.id).attention.length : 0
  return card(LOCAL_NODE, "is-local", () => go(nodeHref(LOCAL_NODE)),
    <><span className="dmap-l1"><StatusDot status={s} /><SquareTerminalIcon className="dmap-ico" aria-hidden /><span className="dmap-name">{t("devices:map.localNode")}</span>{s.kind !== "online" ? <StatusBadge status={s} /> : <Chip>{t("devices:map.builtIn")}</Chip>}</span>
      <span className="dmap-l2"><Sessions v={v} />{lp != null && <span>{t("devices:map.cpu", { n: lp })}</span>}{mp != null && <span>{t("devices:map.mem", { n: mp })}</span>}{ask > 0 && <span className="text-attention">{t("devices:map.asks", { count: ask })}</span>}</span></>,
    <NodeHover v={v} />, { "data-map-local": "" })
}

/** "会话 2/6" and its small bar */
function Sessions({ v }: { v: NodeView }) {
  const t = useT()
  if (v.max == null || v.running == null || !v.linked || v.state === "pending") return null
  const full = v.max > 0 && v.running >= v.max
  return <span className="dmap-sessions">{t("devices:map.sessions", { n: v.running, max: v.max })}<span className="dmap-bar" aria-hidden><i className={full ? "is-full" : undefined} style={{ width: `${v.max ? Math.min(100, (v.running / v.max) * 100) : 0}%` }} /></span></span>
}

/** a remote Node: two lines (up to six of them), one line from seven on */
function NodeMapCard({ v, compact, narrow, card }: { v: NodeView; compact: boolean; narrow: boolean; card: CardFn }) {
  const t = useT(), { hubVersion, vitals } = useDevices()
  const s = nodeStatus(v, hubVersion), l = v.linked ? vitals?.[v.id]?.latest : undefined, lp = loadPct(l), rtt = v.linked ? rttOf(v, l) : null
  const route = linkRoute(v), host = hostOf(v, vitals?.[v.id]?.latest)
  const tone = { warn: "is-warn", bad: "is-bad", info: "is-info", off: "is-off" }[s.tone as "warn"] ?? ""
  const version = v.version && hubVersion && v.version !== hubVersion ? <span className="dmap-version" title={t("devices:map.hover.hubIs", { v: hubVersion })}>{v.version}</span> : null
  const data = { "data-map-node": v.id, "data-map-status": s.kind }
  if (compact) return card(v.id, cn("is-compact", tone), () => go(nodeHref(v.id)),
    <span className="dmap-l1"><StatusDot status={s} /><span className="dmap-name">{v.name}</span>
      <span className="dmap-aside">{s.kind === "online" ? <Sessions v={v} /> : <span className={cn("dmap-state", tone)}>{statusText(s)}</span>}{version}</span></span>,
    <NodeHover v={v} />, data)
  return card(v.id, tone, () => go(nodeHref(v.id)),
    <><span className="dmap-l1"><StatusDot status={s} /><span className="dmap-ico" aria-hidden>{hostIcon(host.kind)}</span><span className="dmap-name">{v.name}</span>{s.kind !== "online" && <StatusBadge status={s} />}</span>
      <span className="dmap-l2"><Sessions v={v} />{lp != null && v.state !== "pending" && <span>{t("devices:map.cpu", { n: lp })}</span>}
        {!narrow && rtt != null && <span>{rtt} ms</span>}{narrow && route && <span>{t(`devices:route.${route}`)}</span>}{version}</span></>,
    <NodeHover v={v} />, data)
}

/** a hover card's rows: a term and its value */
function Facts({ title, rows, hint }: { title: ReactNode; rows: [string, ReactNode][]; hint?: string }) {
  return <div className="flex flex-col gap-1.5" data-map-hover>
    <div className="flex items-center gap-2 type-callout font-medium text-foreground">{title}</div>
    <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-0.5 type-footnote">
      {rows.map(([k, val]) => <div key={k} className="contents"><dt className="text-muted-foreground">{k}</dt><dd className="text-right tabular-nums text-foreground [overflow-wrap:anywhere]">{val}</dd></div>)}
    </dl>
    {hint && <div className="type-footnote text-tertiary">{hint}</div>}
  </div>
}

function NodeHover({ v }: { v: NodeView }) {
  const t = useT(), { hubVersion, vitals } = useDevices()
  const s = nodeStatus(v, hubVersion), l = v.linked ? vitals?.[v.id]?.latest : undefined, lp = loadPct(l), mp = memPct(l), rtt = v.linked ? rttOf(v, l) : null
  const route = linkRoute(v), host = hostLine(hostOf(v, vitals?.[v.id]?.latest))
  const rows: [string, ReactNode][] = [[t("devices:map.hover.status"), statusText(s)]]
  rows.push([t("devices:map.hover.link"), [route ? t(`devices:route.${route}`) + (v.node?.link?.tls ? " · TLS" : "") : v.local ? t("devices:route.local") : t("devices:map.routeUnknown"), rtt != null ? `${rtt} ms` : ""].filter(Boolean).join(" · ")])
  if (host) rows.push([t("devices:card.machine"), host])
  if (v.max != null && v.running != null) rows.push([t("devices:map.hover.sessions"), t("devices:map.hover.sessionsVal", { n: v.running, max: v.max }) + ((v.waiting ?? 0) > 0 ? t("devices:map.hover.queuedVal", { n: v.waiting }) : "")])
  if (lp != null && l?.load && l.cpus) rows.push([t("devices:vitals.cpu"), t("devices:map.hover.cpuVal", { pct: lp, load: num(one(l.load[0])), cpus: l.cpus })])
  if (mp != null && l?.mem_total && l.mem_available != null) rows.push([t("devices:vitals.mem"), t("devices:map.hover.memVal", { pct: mp, used: num((l.mem_total - l.mem_available) / GIB, { maximumFractionDigits: 1 }), total: num(l.mem_total / GIB, { maximumFractionDigits: 1 }) })])
  if (v.version) rows.push([t("devices:card.version"), v.version + " · " + (!hubVersion || v.version === hubVersion ? t("devices:upgrade.same") : t("devices:map.hover.hubIs", { v: hubVersion }))])
  if (offlineReason(v)) rows.push([t("devices:map.hover.error"), <span className="text-destructive">{offlineReason(v)}</span>])
  return <Facts title={<><StatusDot status={s} />{v.local ? t("devices:map.localNode") : v.name}</>} rows={rows} hint={t("devices:map.hover.open")} />
}

function HubHover({ nodesOff, policy }: { nodesOff: boolean; policy?: string }) {
  const t = useT(), { hubVersion } = useDevices()
  const rows: [string, ReactNode][] = []
  if (hubVersion) rows.push([t("devices:card.version"), hubVersion])
  rows.push([t("devices:hub.accept"), nodesOff ? t("devices:hub.off") : t("devices:hub.on")])
  if (policy) rows.push([t("devices:map.hover.upgradeDefault"), t(`device:upgrade.policy.${policy as "auto"}`)])
  return <Facts title={<><NetworkIcon className="size-4 text-muted-foreground" aria-hidden />Hub</>} rows={rows} hint={t("devices:map.hover.hubOpen")} />
}

function ClientHover({ c, me, on }: { c: AccessClient; me: boolean; on: boolean }) {
  const t = useT(), { access } = useDevices()
  const token = c.token_id ? access?.tokens.find((x) => x.id === c.token_id)?.name : undefined
  const rows: [string, ReactNode][] = [[t("devices:map.hover.kind"), [t(`devices:clients.kind.${clientKind(c.kind, c.platform)}`), c.platform].filter(Boolean).join(" · ")]]
  if (c.last_route) rows.push([t("devices:map.hover.via"), routeLabel(c.last_route)])
  rows.push([t("devices:map.hover.status"), on ? t("devices:status.online") : c.last_seen ? t("devices:clients.lastVisit", { ago: agoText(c.last_seen) }) : t("devices:tokens.neverUsed")])
  if (c.version) rows.push([t("devices:card.version"), c.version])
  if (token) rows.push([t("devices:map.hover.signIn"), t("devices:clients.token", { name: token })])
  return <Facts title={c.name} rows={rows} hint={me ? t("devices:map.hover.thisClient") : t("devices:map.hover.clientOpen")} />
}
