type Ran = "ok" | "skip" | "fail" | "wait"
export function traceOf(steps: { node_id: string; status: string }[]) {
  const path: string[] = [], ran: Record<string, Ran> = {}
  for (const s of steps) { if (path[path.length - 1] !== s.node_id) path.push(s.node_id); ran[s.node_id] = s.status === "failed" ? "fail" : s.status === "waiting" ? "wait" : s.status === "skipped" ? "ok" : "ok" }
  return { path, ran }
}
