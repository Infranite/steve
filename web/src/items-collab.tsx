/* ── the items page's 协作 view (affectUI Agent Fleet) ──
 * Every open item that coordinates a work graph is a main agent: Steve's coordinator, its state the item's own one
 * state (item-state.ts). Open one and its steps hang beside it – each with its Bot, its state, cost and working time
 * (GET /stats/tree); a step waiting for you shows what it asks, a failed one why. A click on a step opens its child
 * item, where its decision or its retry is. No answers are taken here: deciding and retrying stay on the item pages. */
import { useEffect, useState } from "react"
import { AgentFleet, type FleetAgent, type FleetAgentStatus, type FleetLabels } from "@/components/affect/agent-fleet-clean"
import { stats as statsApi } from "@/api/stats"
import { useT } from "@/i18n"
import { clockDur } from "@/i18n/format"
import { itemState } from "@/item-state"
import { fmtUsd } from "@/stats/format"
import { botName, closedSt, loadItem, st, type Item } from "@/store"
import { BotAvatar, go } from "@/ui"
import { buildAgentTree, isOver, rollup, type AgentNode, type AgentStatus, type TreeItem, type TreeStat } from "@/agent-tree"
import { model, ROOT_STATUS } from "@/agent-tree-view"

const FLEET: Record<AgentStatus, FleetAgentStatus> = {
  done: "done", failed: "failed", stopped: "stopped", running: "running", starting: "running", unconfirmed: "running", asking: "waiting",
  idle: "idle", paused: "paused", proposed: "queued", pending: "queued", waiting: "queued", blocked: "queued",
}
/** an item coordinates a work graph when the Hub says it has one (its cycle), or its loaded detail holds one */
const coordinates = (i: Item) => i.workflowCycle != null || !!i.workflow

type T = ReturnType<typeof useT>
/** Agent Fleet's words, translated */
export function fleetLabels(t: T): Partial<FleetLabels> {
  const S = (k: string, o?: Record<string, unknown>): string => (t as unknown as (k: string, o?: object) => string)(`views:items.fleet.${k}`, o)
  const status = (s: FleetAgentStatus) => t(`views:agentTree.status.${({ queued: "pending", running: "running", waiting: "asking", done: "done", failed: "failed", stopped: "stopped", idle: "idle", paused: "paused" } as const)[s]}`)
  return { status: { queued: status("queued"), running: status("running"), waiting: status("waiting"), done: status("done"), failed: status("failed"), stopped: status("stopped"), idle: status("idle"), paused: status("paused") },
    noSubs: S("noSubs"), needsYou: (n) => S("needsYou", { count: n }), failed: (n) => S("failed", { count: n }), running: (n, of) => S("running", { n, of }), allDone: (n) => S("allDone", { count: n }), subs: (n) => S("subs", { count: n }),
    agents: (n) => S("agents", { count: n }), subsOf: (name) => S("subsOf", { name }), branch: S("branch"), noSubsYet: S("noSubs"), noAgents: S("empty"),
    runningN: (n) => S("runningN", { count: n }), needsYouN: (n) => S("needsYouN", { count: n }), done: status("done"), runningWord: status("running") }
}

export function ItemsFleet({ xs }: { xs: Item[] }) {
  const t = useT()
  const mains = xs.filter((i) => !i.arch && !closedSt(i.st) && coordinates(i))
  const ids = mains.map((i) => i.id).join()
  /* the graphs live in the items' details, the figures in their stats trees: both load when the view opens */
  useEffect(() => { for (const i of mains) if (!i.detail) void loadItem(i.id) }, [ids])
  const [figs, setFigs] = useState<Record<string, TreeStat[]>>({})
  useEffect(() => {
    let on = true
    for (const i of mains) statsApi.tree(i.id).then((r) => { if (on) setFigs((f) => ({ ...f, [i.id]: r.nodes })) }).catch(() => {})
    return () => { on = false }
  }, [ids])
  /* the first card (what needs you first) is open until another is picked */
  const [open, setOpen] = useState<string | null | undefined>(undefined)
  const S = (k: string, o?: Record<string, unknown>): string => (t as unknown as (k: string, o?: object) => string)(`views:items.fleet.${k}`, o)
  const agents: FleetAgent[] = mains.map((i) => {
    const rows = figs[i.id] ?? [], by = new Map(rows.map((r) => [r.item_id, r])), total = rollup(rows).get(i.id)
    const state = itemState(i, i.workflow, i.ss.some((k) => st.S[k]?.st === "run"))
    const tree = buildAgentTree(i.id, st.items as TreeItem[], i.workflow, 8, ROOT_STATUS[state.kind])
    const steps = tree ? tree.children.filter((c) => c.kind === "unit") : []
    const sub = (n: AgentNode): FleetAgent => {
      const tries = tree!.children.filter((c) => c.kind === "attempt" && c.unitId === n.unitId)
      const items = [...tries, n].map((x) => x.item).filter((x): x is string => !!x)
      const own = items.map((x) => by.get(x)?.est ?? 0).reduce((a, b) => a + b, 0)
      const worked = items.map((x) => by.get(x)?.agent_ms ?? 0).reduce((a, b) => a + b, 0)
      const needs = n.decisions > 0 && !isOver(n.status)
      const child = n.item ? st.items.find((x) => x.id === n.item) : undefined
      return {
        id: `${i.id}:${n.key}`, name: n.bot ? botName(n.bot) : t("views:agentTree.noBot"), model: model(n.bot) || undefined, avatar: <BotAvatar k={n.bot} size={20} />,
        task: n.title, status: needs ? "waiting" : FLEET[n.status], statusText: t(`views:agentTree.status.${needs ? "asking" : n.status}`), cost: own || undefined, time: worked ? clockDur(worked) : "",
        ask: needs ? child?.waits[0]?.q ?? t("views:agentTree.row.needsYouBare") : undefined, error: n.status === "failed" ? n.reason ?? undefined : undefined,
        attrs: { "data-fleet-step": n.key, "data-fleet-item": n.item },
      }
    }
    const coord = by.get(i.id), status: FleetAgentStatus = tree ? FLEET[tree.status] : "queued"
    const children = steps.map(sub)
    /* the branch's total is everything under the item (earlier rounds and forks too); the steps show their own part */
    const rest = total ? total.est - children.reduce((n, c) => n + (c.cost ?? 0), 0) : coord?.est
    return {
      id: i.id, name: i.t, model: `${i.id} · ${botName(i.b)}`, avatar: <BotAvatar k={i.b} size={20} />, task: i.now || i.goals.at(-1) || "",
      status, statusText: t(`views:itemState.${state.kind}`), cost: rest && rest > 0 ? rest : undefined, time: coord?.agent_ms ? clockDur(coord.agent_ms) : "",
      children, attrs: { "data-fleet-main": i.id },
    }
  })
  /* what needs you first, then what fails, then what runs */
  const rank = (a: FleetAgent) => { const all = a.children ?? []; return all.some((c) => c.status === "waiting") || a.status === "waiting" ? 0 : all.some((c) => c.status === "failed") ? 1 : a.status === "running" || all.some((c) => c.status === "running") ? 2 : 3 }
  const sorted = [...agents].sort((a, b) => rank(a) - rank(b))
  return <div className="min-w-0" data-items-fleet>
    <AgentFleet agents={sorted} open={open !== undefined && sorted.some((a) => a.id === open) || open === null ? open : sorted[0]?.id ?? null} onOpenChange={setOpen} title={S("title")} formatCost={fmtUsd}
      onSelect={(a) => { const item = a.attrs?.["data-fleet-item"]; if (item) go(`#/items/${encodeURIComponent(item)}`) }}
      labels={fleetLabels(t)} />
  </div>
}
