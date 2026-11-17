/* What the devices pages read, once for all of their tabs: the device rows, the Node registry and the access view.
 * Each keeps its last good answer (also across tab changes, which remount the page) and says when a later read
 * failed, so a failure is never shown as an empty list. Nodes and devices are read every 10 seconds while the page
 * is visible; there is no refresh button. */
import { createContext, useContext, useEffect, useMemo, useState, type ReactNode } from "react"
import { api, type RDevice, type RNode } from "@/api"
import { access, type AccessView } from "@/access-api"
import { nodeViews, type NodeView } from "@/device-model"
import { st, useStore } from "@/store"

const EVERY = 10_000
type NodesRead = { nodes: RNode[]; off: boolean }
let lastNodes: NodesRead | null = null, lastDevices: RDevice[] | null = null, lastAccess: AccessView | null = null

/** reads `load` now, again every 10 s while the page is visible, and at once when it becomes visible again */
function usePoll<T>(load: () => Promise<T>, initial: T | null, keep: (v: T) => void, deps: unknown[] = []) {
  const [value, setValue] = useState<T | null>(initial), [error, setError] = useState(""), [rev, setRev] = useState(0)
  useEffect(() => {
    let live = true, busy = false, timer: ReturnType<typeof setTimeout> | undefined
    const read = async () => {
      if (!live || busy) return
      if (document.hidden) { timer = setTimeout(() => void read(), EVERY); return }
      busy = true
      try { const v = await load(); if (live) { keep(v); setValue(v); setError("") } }
      catch (e) { if (live) setError((e as Error).message) }
      finally { if (live) { busy = false; timer = setTimeout(() => void read(), EVERY) } }
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
  reloadNodes: () => void
  reloadAccess: () => void
}
const Ctx = createContext<DevicesData | null>(null)

export function DevicesProvider({ children }: { children: ReactNode }) {
  useStore()
  const nodes = usePoll(async () => { const r = await api.nodes(); return { nodes: r.nodes, off: !!r.off } }, lastNodes, (v) => { lastNodes = v })
  const devices = usePoll(async () => (await api.devices()).devices, lastDevices, (v) => { lastDevices = v })
  /* the owner lists every member's clients; the Hub answers anyone else with their own */
  const acc = usePoll(() => access.get(st.owner), lastAccess, (v) => { lastAccess = v }, [st.owner])
  const hubVersion = st.version
  const ds = devices.value ?? st.devices
  const views = useMemo(() => nodeViews(ds, nodes.value?.nodes ?? null, hubVersion), [ds, nodes.value, hubVersion])
  const value: DevicesData = {
    views, nodesRead: nodes.value != null, nodesError: nodes.error, nodesOff: !!nodes.value?.off, devicesError: devices.error,
    access: acc.value, accessError: acc.error, hubVersion,
    reloadNodes: () => { nodes.reload(); devices.reload() }, reloadAccess: acc.reload,
  }
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>
}

export function useDevices(): DevicesData {
  const v = useContext(Ctx)
  if (!v) throw new Error("useDevices outside DevicesProvider")
  return v
}
