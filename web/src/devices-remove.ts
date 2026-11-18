/* Removing a paired Node from its machine (#325): what the Node page's "Remove from this machine" dialog shows.
 * The Hub decides whether it can ask the Node (it is linked and says it can remove itself); the page only shows it.
 * Asked, the Node stops taking work, lets what runs end (or cancels it past the grace period), uninstalls its service and
 * deletes its home once the Hub confirms; the Hub then unpairs it. Any other Node (offline, of an earlier release, one
 * that did not confirm) is removed on its machine with the command the Hub gives. */
import type { RNode, RNodeRemoval, RNodeRemovalRun } from "@/api"

/** The Hub's own command, for a Hub that sends none: stop and uninstall the service, then delete the default home. */
export const MANUAL_REMOVE = `S=$(command -v steve || ls ~/.steve-node/releases/*/bin/steve | tail -n 1) && "$S" node stop && "$S" node uninstall && rm -rf ~/.steve-node`

/** the steps a removal shows, in order */
export const REMOVE_STEPS = ["draining", "ready", "removing", "unpaired"] as const
export type RemoveStep = typeof REMOVE_STEPS[number]

/** phases after which nothing more happens to a removal */
export const REMOVE_DONE = ["removed", "unconfirmed", "failed", "manual"] as const

export type RemoveView =
  /** nothing to remove here: the Hub's own Node, one already unpaired, or a Hub that cannot say */
  | { kind: "none" }
  /** the Hub can ask the Node: confirm first. `last`: how the previous attempt ended without removing it */
  | { kind: "ask"; grace: number; last: RNodeRemovalRun | null; command: string }
  /** the Hub cannot ask it (`offline`, or `manual`: it cannot remove itself): the command to run on its machine */
  | { kind: "manual"; why: "offline" | "manual"; command: string }
  /** under way: the step it is on */
  | { kind: "running"; run: RNodeRemovalRun; step: RemoveStep }
  /** it ended without removing the Node (`unconfirmed`, `failed`): why, and the command to run on its machine */
  | { kind: "ended"; run: RNodeRemovalRun; command: string }
  /** removed from its machine and unpaired */
  | { kind: "removed"; run: RNodeRemovalRun }

export const removeDone = (phase: string) => (REMOVE_DONE as readonly string[]).includes(phase)

/** which step a removal under way is on */
export function removeStep(phase: string): RemoveStep {
  switch (phase) {
    case "ready": return "ready"
    case "confirming": return "removing"
    case "removed": return "unpaired"
    default: return "draining"
  }
}

/** What the dialog shows for a Node. `refused`: what the Hub answered when asked to start (`offline`, `manual`,
 *  `busy`), which wins over what the last list said. `started`: the operation this dialog started, whose end it shows. */
export function removeView(n: Pick<RNode, "local" | "state" | "removal"> | null | undefined, refused?: string | null, started?: string | null): RemoveView {
  const r: RNodeRemoval | null | undefined = n?.removal
  if (!n || n.local || !r) return { kind: "none" }
  const command = r.command || MANUAL_REMOVE
  const last = r.latest ?? null
  if (last && !removeDone(last.phase)) return { kind: "running", run: last, step: removeStep(last.phase) }
  /* just started here, before the list says so: under way, never the question again */
  if (started && last?.operation_id !== started) return { kind: "running", run: { operation_id: started, phase: "requested", requested_by: "", grace_secs: r.grace_secs ?? 0, created: 0, updated: 0 }, step: "draining" }
  if (last && started && last.operation_id === started) {
    if (last.phase === "removed") return { kind: "removed", run: last }
    if (last.phase === "manual") return { kind: "manual", why: "manual", command }
    return { kind: "ended", run: last, command }
  }
  if (n.state !== "active" && n.state !== "paused") return last?.phase === "removed" ? { kind: "removed", run: last } : { kind: "none" }
  const why = refused === "offline" || refused === "manual" ? refused : r.reason === "offline" || r.reason === "manual" ? r.reason : null
  if (why) return { kind: "manual", why, command }
  return { kind: "ask", grace: r.grace_secs ?? 0, last: last && last.phase !== "removed" ? last : null, command }
}

/** each step's state for a removal under way: done, the current one, still to come */
export function removeSteps(step: RemoveStep): ("done" | "active" | "todo")[] {
  const at = REMOVE_STEPS.indexOf(step)
  return REMOVE_STEPS.map((_, i) => (i < at ? "done" : i === at ? "active" : "todo"))
}
