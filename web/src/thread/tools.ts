/* What one tool call of a research run did, for its label and for the run's summary. Told by the category the
 * harness adapter reports for the call (ACP `kind`: every harness has one, whatever it names its tools) and, for
 * Steve's own tools, by their name. Steps stored before the category was kept carry a tool name only. */
export type ToolClass = "read" | "search" | "exec" | "edit" | "fetch" | "kb" | "item" | "meet" | "other"
export type ToolLike = { name: string; kind?: string; args?: Record<string, unknown> }

/** `mcp__steve__steve_item_get` → `steve_item_get`: the name without the MCP server it came through */
export const toolShort = (name: string) => name.replace(/^mcp__[^_]+__/, "")
const BY_KIND: Record<string, ToolClass> = { read: "read", search: "search", exec: "exec", edit: "edit", delete: "edit", move: "edit", fetch: "fetch" }
const BY_NAME: Record<string, ToolClass> = { Read: "read", Grep: "search", Glob: "search", LS: "search", Bash: "exec" }

export function toolClass(name: string, kind?: string): ToolClass {
  const n = toolShort(name)
  if (n.startsWith("steve_onboard_")) return "meet"
  if (n.includes("knowledge")) return "kb"
  if (n.includes("steve_item")) return "item"
  return BY_KIND[kind ?? ""] ?? BY_NAME[name] ?? "other"
}
/** `profile` | `models` | `bots` | `done` for a first-meeting tool, else "" */
export type MeetStep = "profile" | "models" | "bots" | "done"
export const meetStep = (name: string) => (/^steve_onboard_(profile|models|bots|done)$/.exec(toolShort(name))?.[1] ?? "") as MeetStep | ""

/** how many calls of each class a run made; `files` is the number of different files read */
export function toolCounts(tools: ToolLike[]) {
  const n: Record<ToolClass, number> = { read: 0, search: 0, exec: 0, edit: 0, fetch: 0, kb: 0, item: 0, meet: 0, other: 0 }
  const files = new Set<string>()
  for (const t of tools) {
    const c = toolClass(t.name, t.kind)
    n[c]++
    if (c === "read") files.add(String(t.args?.brief ?? ""))
  }
  return { ...n, files: files.size }
}
