/* What the devices pages read, once for all of their tabs: the device rows, the Node registry, the access view, every
 * Node's vitals and the Hub's defaults for Nodes (upgrade policy and its waits, personal agent config). Each keeps its
 * last good answer (also across tab changes, which remount the page) and says when a later read failed, so a failure
 * is never shown as an empty list. The Hub's stream says when a Node or a client changed (`node`, `client` events) and
 * they are read again at once; besides, Nodes and devices are read every 10 seconds while the page is visible (a Hub
 * of an earlier release sends no such events), vitals every minute (the Hub samples once a minute). No refresh button. */
import { createContext, useContext, useEffect, useMemo, useState, type ReactNode } from "react"
import { api, type RDevice, type RNode, type RUpgradeDefault, type RVitals } from "@/api"
import { access, type AccessView } from "@/access-api"
import { nodeViews, type NodeView } from "@/device-model"
import { st, useHubEvents, useStore } from "@/store"

const EVERY = 10_000, VITALS_EVERY = 60_000
type NodesRead = { nodes: RNode[]; off: boolean }
type Defaults = { upgrade: RUpgradeDefault | null; context: "on" | "off" | null }
let lastNodes: NodesRead | null = null, lastDevices: RDevice[] | null = null, lastAccess: AccessView | null = null
let lastVitals: Record<string, RVitals> | null = null, lastDefaults: Defaults | null = null

/** reads `load` now, again every `every` ms (10 s) while the page is visible, and at once when it becomes visible again */
function usePoll<T>(load: () => Promise<T>, initial: T | null, keep: (v: T) => void, deps: unknown[] = [], every = EVERY) {
  const [value, setValue] = useState<T | null>(initial), [error, setError] = useState(""), [rev, setRev] = useState(0)
  useEffect(() => {
    let live = true, busy = false, timer: ReturnType<typeof setTimeout> | undefined
    const read = async () => {
      if (!live || busy) return
      if (document.hidden) { timer = setTimeout(() => void read(), every); return }
      busy = true
      try { const v = await load(); if (live) { keep(v); setValue(v); setError("") } }
      catch (e) { if (live) setError((e as Error).message) }
      finally { if (live) { busy = false; timer = setTimeout(() => void read(), every) } }
    }
    const visible = () => { if (!document.hidden && !busy) { clearTimeout(timer); void read() } }
    void read()
    document.addEventListener("visibilitychange", visible)
    return () => { live = false; clearTimeout(timer); document.removeEventListener("visibilitychange", visible) }
  }, [rev, ...deps])
  return { value, error, reload: () => setRev((x) => x + 1) }
}

export type DevicesData = {
  /** every Node, the Hub's own first; null until the registry was read once */
  views: NodeView[]
  nodesRead: boolean
  nodesError: string
  /** the Hub does not listen for remote Nodes */
  nodesOff: boolean
  devicesError: string
  access: AccessView | null
  accessError: string
  hubVersion: string
  /** every Node's day of samples, by Node id; null until read once */
  vitals: Record<string, RVitals> | null
  vitalsError: string
  /** the Hub's upgrade policy for Nodes and how long an upgrade waits; null when it could not be read */
  upgradeDefault: RUpgradeDefault | null
  /** the Hub's default for loading a Node owner's personal agent config; null when it could not be read */
  contextDefault: "on" | "off" | null
  reloadNodes: () => void
  reloadAccess: () => void
  reloadDefaults: () => void
}
const Ctx = createContext<DevicesData | null>(null)

export function DevicesProvider({ children }: { children: ReactNode }) {
  useStore()
  const nodes = usePoll(async () => { const r = await api.nodes(); return { nodes: r.nodes, off: !!r.off } }, lastNodes, (v) => { lastNodes = v })
  const devices = usePoll(async () => (await api.devices()).devices, lastDevices, (v) => { lastDevices = v })
  /* the owner lists every member's clients; the Hub answers anyone else with their own */
  const acc = usePoll(() => access.get(st.owner), lastAccess, (v) => { lastAccess = v }, [st.owner])
  const vit = usePoll(async () => (await api.vitals()).nodes, lastVitals, (v) => { lastVitals = v }, [], VITALS_EVERY)
  /* the defaults change only when someone sets them: read once, again after a change; each may be refused on its own */
  const defs = usePoll<Defaults>(async () => {
    const [upgrade, context] = await Promise.all([api.nodeUpgradeDefault().catch(() => null), api.nodeContextDefault().then((r) => r.owner_context, () => null)])
    return { upgrade, context }
  }, lastDefaults, (v) => { lastDefaults = v }, [], 10 * 60_000)
  useHubEvents(["node", "device"], () => { nodes.reload(); devices.reload() })
  useHubEvents(["client"], () => acc.reload())
  const hubVersion = st.version
  const ds = devices.value ?? st.devices
  const views = useMemo(() => nodeViews(ds, nodes.value?.nodes ?? null, hubVersion), [ds, nodes.value, hubVersion])
  const value: DevicesData = {
    views, nodesRead: nodes.value != null, nodesError: nodes.error, nodesOff: !!nodes.value?.off, devicesError: devices.error,
    access: acc.value, accessError: acc.error, hubVersion, vitals: vit.value, vitalsError: vit.error,
    upgradeDefault: defs.value?.upgrade ?? null, contextDefault: defs.value?.context ?? null,
    reloadNodes: () => { nodes.reload(); devices.reload() }, reloadAccess: acc.reload, reloadDefaults: defs.reload,
  }
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>
}

export function useDevices(): DevicesData {
  const v = useContext(Ctx)
  if (!v) throw new Error("useDevices outside DevicesProvider")
  return v
}

/** one Node's samples: undefined while unread, null when the Hub has none for it; stale past the Hub's freshness window */
export function useNodeVitals(id: string) {
  const { vitals, vitalsError } = useDevices()
  const value = vitals ? vitals[id] ?? null : undefined
  const stale = !!value?.sampling && Date.now() - value.latest.at > value.sampling.stale_after_ms
  return { value, stale, error: vitalsError }
}
