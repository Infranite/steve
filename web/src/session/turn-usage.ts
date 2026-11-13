/* A finished turn's tokens and estimated cost for its line in the transcript. The Hub keeps usage per turn for an
 * item's sessions (`/stats/items/:id`); every turn line of one item shares one request, kept for a short while, and a
 * turn is matched by when it started and ended, as the timeline view and the session panel match it. */
import { useEffect, useState } from "react"
import { stats, type ItemDetail } from "@/api/stats"
import { st } from "@/store"
import type { TurnUsage } from "./data"

const cache = new Map<string, { at: number; p: Promise<ItemDetail | null> }>()
const KEEP = 15_000
function itemStats(id: string) {
  const c = cache.get(id)
  if (c && Date.now() - c.at < KEEP) return c.p
  const p = stats.item(id).catch(() => null)
  cache.set(id, { at: Date.now(), p })
  return p
}
/** the usage of the turn of session `k` that ran from `t0` to `t1`; undefined while unknown or for a live turn */
export function useTurnUsage(k: string, t0?: number, t1?: number, live = false): TurnUsage | undefined {
  const item = st.S[k]?.item ?? null
  const [u, setU] = useState<TurnUsage | undefined>()
  useEffect(() => {
    if (!item || live || !t0 || !t1) { setU(undefined); return }
    let on = true
    void itemStats(item).then((d) => {
      const m = d?.turns?.find((x) => x.session_id === k && Math.abs(x.started - t0) <= 5000 && x.ended != null && Math.abs(x.ended - t1) <= 5000)
      if (on) setU(m?.usage)
    })
    return () => { on = false }
  }, [item, k, t0, t1, live])
  return u
}
