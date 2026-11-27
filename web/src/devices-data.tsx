/* What the devices pages read, once for all of their tabs: the device rows, the Node registry, the access view, every
 * Node's vitals, the Hub's health (owner only), and the Hub's defaults for Nodes (upgrade policy and its waits,
 * personal agent config). Each keeps its last good answer (also across tab changes, which remount the page) and says
 * when a later read failed, so a failure is never shown as an empty list. The Hub's stream says when a Node or a
 * client changed (`node`, `client` events) and they are read again at once. Nodes and devices refresh every 10 seconds
 * while visible for live counters that lack change events. Per-Node harness reports are read with them and can be
 * explicitly reprobed on a Node's page; vitals and health are read every minute (the Hub samples once a minute). */
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react"
import { useQuery } from "@tanstack/react-query"
import { api, HubError, type RDevice, type RNodeHarnessReport, type RUpgradeDefault, type RVitals } from "@/api"
import { access, type AccessView } from "@/api/access"
import { systemApi } from "@/api/system"
import { qc, scopeKey } from "@/data/sync"
import { nodeViews, type NodeView } from "@/device-model"
import type { HubHealth } from "@/health"
import { st, useHubEvents, useStore } from "@/store"

const EVERY = 10_000, VITALS_EVERY = 60_000
type Defaults = { upgrade: RUpgradeDefault | null; context: "on" | "off" | null }
let lastDevices: RDevice[] | null = null, lastAccess: AccessView | null = null
let lastVitals: Record<string, RVitals> | null = null, lastDefaults: Defaults | null = null
let lastHarnesses: Record<string, RNodeHarnessReport> | null = null
let lastHealth: HubHealth | null = null

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
  const replace = useCallback((v: T) => { keep(v); setValue(v) }, []) // eslint-disable-line react-hooks/exhaustive-deps
  const reload = useCallback(() => setRev((x) => x + 1), [])
  return { value, error, replace, reload }
}

export type DevicesData = {
  /** every Node, the Hub's own first; null until the registry was read once */
  views: NodeView[]
  nodesRead: boolean
  nodesError: string
  harnesses: Record<string, RNodeHarnessReport> | null
  harnessesError: string
  refreshHarnesses: (id: string) => Promise<void>
  /** the Hub does not listen for remote Nodes */
  nodesOff: boolean
  devicesError: string
  access: AccessView | null
  accessError: string
  hubVersion: string
  /** every Node's day of samples, by Node id; null until read once */
  vitals: Record<string, RVitals> | null
  vitalsError: string
  /** Hub system health (15m summary for owner only); null for non-owners or before first read */
  health: HubHealth | null
  healthError: string
  /** the Hub's upgrade policy for Nodes and how long an upgrade waits; null when it could not be read */
  upgradeDefault: RUpgradeDefault | null
  /** the Hub's default for loading a Node owner's personal agent config; null when it could not be read */
  contextDefault: "on" | "off" | null
  reloadNodes: () => void
  reloadAccess: () => void
  reloadDefaults: () => void
  reloadHealth: () => void
  replaceHealth: (v: HubHealth) => void
}
const Ctx = createContext<DevicesData | null>(null)

export function DevicesProvider({ children }: { children: ReactNode }) {
  useStore()
  const { data: nodes, error: nodesError, refetch: readNodes } = useQuery({
    queryKey: ["detail", scopeKey(), "nodes"],
    queryFn: api.nodes,
    staleTime: 0,
    refetchInterval: EVERY,
  }, qc)
  useEffect(() => {
    const visible = () => { if (!document.hidden) void readNodes() }
    document.addEventListener("visibilitychange", visible)
    return () => document.removeEventListener("visibilitychange", visible)
  }, [readNodes])
  const devices = usePoll(async () => (await api.devices()).devices, lastDevices, (v) => { lastDevices = v })
  const harnesses = usePoll(async () => (await api.nodeHarnesses()).nodes, lastHarnesses, (v) => { lastHarnesses = v })
  /* the owner lists every member's clients; the Hub answers anyone else with their own */
  const acc = usePoll(() => access.get(st.owner), lastAccess, (v) => { lastAccess = v }, [st.owner])
  const vit = usePoll(async () => (await api.vitals()).nodes, lastVitals, (v) => { lastVitals = v }, [], VITALS_EVERY)
  /* system health is owner-only; non-owners issue zero /api/system/health requests and 403 stays quiet */
  const [healthState, setHealthState] = useState<{ value: HubHealth | null; error: string }>(() => ({
    value: st.owner ? lastHealth : null,
    error: "",
  }))
  const [healthRev, setHealthRev] = useState(0)
  useEffect(() => {
    if (!st.owner) {
      lastHealth = null
      setHealthState({ value: null, error: "" })
      return
    }
    let live = true, busy = false, timer: ReturnType<typeof setTimeout> | undefined
    const read = async () => {
      if (!live || busy) return
      if (document.hidden) { timer = setTimeout(() => void read(), VITALS_EVERY); return }
      busy = true
      try {
        const v = await systemApi.health("15m")
        if (live) { lastHealth = v; setHealthState({ value: v, error: "" }) }
      } catch (e) {
        if (!live) return
        if (e instanceof HubError && e.status === 403) {
          lastHealth = null
          setHealthState({ value: null, error: "" })
        } else {
          setHealthState((prev) => ({ value: prev.value, error: (e as Error).message }))
        }
      } finally {
        if (live) { busy = false; timer = setTimeout(() => void read(), VITALS_EVERY) }
      }
    }
    const visible = () => { if (!document.hidden && !busy) { clearTimeout(timer); void read() } }
    void read()
    document.addEventListener("visibilitychange", visible)
    return () => { live = false; clearTimeout(timer); document.removeEventListener("visibilitychange", visible) }
  }, [st.owner, healthRev])
  const reloadHealth = useCallback(() => setHealthRev((x) => x + 1), [])
  const replaceHealth = useCallback((v: HubHealth) => {
    lastHealth = v
    setHealthState({ value: v, error: "" })
  }, [])
  /* the defaults change only when someone sets them: read once, again after a change; each may be refused on its own */
  const defs = usePoll<Defaults>(async () => {
    const [upgrade, context] = await Promise.all([api.nodeUpgradeDefault().catch(() => null), api.nodeContextDefault().then((r) => r.owner_context, () => null)])
    return { upgrade, context }
  }, lastDefaults, (v) => { lastDefaults = v }, [], 10 * 60_000)
  useHubEvents(["node", "device"], () => { void readNodes(); devices.reload(); harnesses.reload() })
  useHubEvents(["client"], () => acc.reload())
  const hubVersion = nodes?.hub_version || st.version || ""
  const ds = devices.value ?? []
  const views = useMemo(() => nodeViews(ds, nodes?.nodes ?? null, hubVersion), [ds, nodes, hubVersion])
  const harnessesRef = useRef(harnesses.value)
  harnessesRef.current = harnesses.value
  const refreshHarnesses = useCallback(async (id: string) => {
    const r = await api.refreshNodeHarnesses(id)
    harnesses.replace({ ...(harnessesRef.current ?? {}), [id]: r.report })
    harnesses.reload()
  }, [harnesses.replace, harnesses.reload])
  const reloadNodes = useCallback(() => { void readNodes(); devices.reload() }, [readNodes, devices.reload])
  const value: DevicesData = {
    views, nodesRead: nodes != null, nodesError: nodesError?.message ?? "", nodesOff: !!nodes?.off, devicesError: devices.error,
    harnesses: harnesses.value, harnessesError: harnesses.error,
    refreshHarnesses,
    access: acc.value, accessError: acc.error, hubVersion, vitals: vit.value, vitalsError: vit.error,
    health: healthState.value, healthError: healthState.error,
    upgradeDefault: defs.value?.upgrade ?? null, contextDefault: defs.value?.context ?? null,
    reloadNodes, reloadAccess: acc.reload, reloadDefaults: defs.reload, reloadHealth, replaceHealth,
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
